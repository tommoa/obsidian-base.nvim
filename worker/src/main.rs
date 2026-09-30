#![forbid(unsafe_code)]
//! Process entry point that bridges standard I/O to the stateful worker actor.

use obsidian_base_worker::actor::{ActorMessage, WorkerActor};
use obsidian_base_worker::protocol::WorkerEnvelope;
use std::io;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

// Allow the default 1 MiB source limit even when every byte is JSON-escaped.
const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024 * 1024;

enum InputLine {
    Line(String),
    Invalid(&'static str),
    Eof,
}

/// Read one JSON line while draining oversized input without retaining it.
async fn read_input_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> io::Result<InputLine> {
    let mut line = Vec::new();
    let mut too_long = false;
    let mut terminated_by_lf = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() && !too_long {
                return Ok(InputLine::Eof);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let chunk_len = newline.unwrap_or(available.len());
        if !too_long {
            // Retain one extra byte so CR can be excluded if it is part of CRLF.
            if chunk_len <= MAX_REQUEST_LINE_BYTES + 1 - line.len() {
                line.extend_from_slice(&available[..chunk_len]);
            } else {
                too_long = true;
                line.clear();
            }
        }
        reader.consume(chunk_len + usize::from(newline.is_some()));
        if newline.is_some() {
            terminated_by_lf = true;
            break;
        }
    }
    if too_long {
        return Ok(InputLine::Invalid("request line exceeds 8 MiB"));
    }
    if terminated_by_lf && line.last() == Some(&b'\r') {
        line.pop();
    }
    if line.len() > MAX_REQUEST_LINE_BYTES {
        return Ok(InputLine::Invalid("request line exceeds 8 MiB"));
    }
    match String::from_utf8(line) {
        Ok(line) => Ok(InputLine::Line(line)),
        Err(_) => Ok(InputLine::Invalid("request line is not UTF-8")),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let (actor_tx, actor_rx) = mpsc::channel(64);
    // A stalled stdout must backpressure the actor rather than retain arbitrary row copies.
    let (output_tx, mut output_rx) = mpsc::channel::<WorkerEnvelope>(1);
    let reader_tx = actor_tx.clone();
    let reader = tokio::spawn(async move {
        let mut input = BufReader::new(tokio::io::stdin());
        loop {
            match read_input_line(&mut input).await {
                Ok(InputLine::Line(line)) => {
                    if reader_tx.send(ActorMessage::Line(line)).await.is_err() {
                        break;
                    }
                }
                Ok(InputLine::Invalid(message)) => {
                    if reader_tx
                        .send(ActorMessage::InvalidLine(message))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(InputLine::Eof) => {
                    let _ = reader_tx.send(ActorMessage::Eof).await;
                    break;
                }
                Err(error) => {
                    eprintln!("stdin error: {error}");
                    let _ = reader_tx.send(ActorMessage::Eof).await;
                    break;
                }
            }
        }
    });
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(envelope) = output_rx.recv().await {
            // Stdout is the JSON-lines protocol; operational diagnostics must stay on stderr.
            match serde_json::to_vec(&envelope) {
                Ok(mut line) => {
                    line.push(b'\n');
                    if stdout.write_all(&line).await.is_err() || stdout.flush().await.is_err() {
                        break;
                    }
                }
                Err(error) => eprintln!("response serialization error: {error}"),
            }
        }
    });
    let actor = WorkerActor::new(actor_tx, output_tx);
    actor.run(actor_rx).await;
    reader.abort();
    let _ = reader.await;
    let _ = writer.await;
}
