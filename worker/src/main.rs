#![forbid(unsafe_code)]
//! Process entry point that bridges standard I/O to the stateful worker actor.

use obsidian_base_worker::actor::{ActorMessage, WorkerActor};
use obsidian_base_worker::protocol::WorkerEnvelope;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let (actor_tx, actor_rx) = mpsc::channel(64);
    let (output_tx, mut output_rx) = mpsc::unbounded_channel::<WorkerEnvelope>();
    let reader_tx = actor_tx.clone();
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if reader_tx.send(ActorMessage::Line(line)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {
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
