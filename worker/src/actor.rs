//! Serializes typed protocol requests and filesystem notifications around one worker service.

use std::collections::BTreeSet;

use tokio::{
    sync::mpsc,
    time::{Duration, Instant},
};

use crate::{
    protocol::{
        ErrorPayload, EventEnvelope, Request, RequestId, ResponseEnvelope, WorkerEnvelope, decode,
    },
    service::WorkerService,
    watch::{VaultWatcher, WatchMessage},
};

/// Work accepted by the actor from standard input or the vault watcher.
pub enum ActorMessage {
    /// One raw JSON-lines protocol request read from standard input.
    Line(String),
    /// A relevant filesystem change reported by the vault watcher.
    Watch(WatchMessage),
    /// End of standard input, which terminates the worker process.
    Eof,
}

/// Owns mutable worker state and coordinates request, watcher, and output channels.
pub struct WorkerActor {
    service: WorkerService,
    input: mpsc::Sender<ActorMessage>,
    output: mpsc::UnboundedSender<WorkerEnvelope>,
    watcher: Option<VaultWatcher>,
    pending_paths: BTreeSet<String>,
    deadline: Option<Instant>,
}

impl WorkerActor {
    /// Create an actor that emits typed worker envelopes on `output`.
    pub fn new(
        input: mpsc::Sender<ActorMessage>,
        output: mpsc::UnboundedSender<WorkerEnvelope>,
    ) -> Self {
        let event_output = output.clone();
        let service = WorkerService::with_emitter(move |event| {
            let _ = event_output.send(WorkerEnvelope::Event(EventEnvelope { event }));
        });
        Self {
            service,
            input,
            output,
            watcher: None,
            pending_paths: BTreeSet::new(),
            deadline: None,
        }
    }

    /// Process input lines and watcher notifications until EOF or shutdown.
    pub async fn run(mut self, mut receiver: mpsc::Receiver<ActorMessage>) {
        loop {
            let message = if let Some(deadline) = self.deadline {
                tokio::select! {
                    message = receiver.recv() => message,
                    _ = tokio::time::sleep_until(deadline) => {
                        self.flush_watch().await;
                        continue;
                    }
                }
            } else {
                receiver.recv().await
            };
            match message {
                Some(ActorMessage::Line(line)) => {
                    if !self.handle_line(&line).await {
                        break;
                    }
                }
                Some(ActorMessage::Watch(WatchMessage::Changed(paths))) => {
                    for path in paths {
                        if self.pending_paths.len() == 8 {
                            break;
                        }
                        self.pending_paths.insert(path);
                    }
                    self.deadline = Some(Instant::now() + Duration::from_millis(100));
                }
                Some(ActorMessage::Watch(WatchMessage::Error(error))) => {
                    self.service.record_watcher_error(error);
                }
                Some(ActorMessage::Eof) | None => break,
            }
        }
    }

    async fn handle_line(&mut self, line: &str) -> bool {
        let envelope = match decode(line) {
            Ok(envelope) => envelope,
            Err(message) => {
                self.send_error(RequestId::INVALID, "invalid_request", message);
                return true;
            }
        };
        let id = envelope.id;
        let starts_watcher = matches!(envelope.request, Request::Initialize(_));
        let shutdown = matches!(envelope.request, Request::Shutdown(_));
        let result = self
            .blocking_service(move |service| service.handle(envelope.request))
            .await;
        match result {
            Ok(Some(success)) => {
                if starts_watcher {
                    self.start_watcher();
                }
                let _ = self
                    .output
                    .send(WorkerEnvelope::Response(ResponseEnvelope::success(
                        id, success,
                    )));
            }
            Ok(None) => {}
            Err(error) => self.send_error(id, error.code, error.message),
        }
        !shutdown
    }

    fn send_error(&self, id: RequestId, code: impl Into<String>, message: impl Into<String>) {
        let _ = self
            .output
            .send(WorkerEnvelope::Response(ResponseEnvelope::error(
                id,
                ErrorPayload {
                    code: code.into(),
                    message: message.into(),
                },
            )));
    }

    fn start_watcher(&mut self) {
        let Some(root) = self.service.root() else {
            return;
        };
        match VaultWatcher::start(root, self.input.clone()) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(error) => self.service.record_watcher_error(error.to_string()),
        }
    }

    async fn flush_watch(&mut self) {
        self.deadline = None;
        let paths = std::mem::take(&mut self.pending_paths)
            .into_iter()
            .collect();
        if let Err(error) = self
            .blocking_service(move |service| service.reindex_external(paths))
            .await
        {
            self.service
                .record_watcher_error(format!("watcher rescan failed: {}", error.message));
        }
    }

    async fn blocking_service<T: Send + 'static>(
        &mut self,
        operation: impl FnOnce(&mut WorkerService) -> T + Send + 'static,
    ) -> T {
        let mut service = std::mem::take(&mut self.service);
        let (service, result) = tokio::task::spawn_blocking(move || {
            let result = operation(&mut service);
            (service, result)
        })
        .await
        .expect("worker blocking task panicked");
        self.service = service;
        result
    }
}
