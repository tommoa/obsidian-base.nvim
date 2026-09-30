//! Serializes typed protocol requests and filesystem notifications around one worker service.

use std::collections::BTreeSet;

use tokio::{
    sync::mpsc,
    time::{Duration, Instant},
};

use crate::{
    environment::ExecutionEnvironment,
    protocol::{
        self, ErrorPayload, Event, EventEnvelope, IndexChangeOrigin, Request, RequestId,
        ResponseEnvelope, WorkerEnvelope,
    },
    service::{IndexChange, WorkerService},
    watch::{VaultWatcher, WatchMessage},
};

/// Work accepted by the actor from standard input or the vault watcher.
pub enum ActorMessage {
    /// One raw JSON-lines protocol request read from standard input.
    Line(String),
    /// A framing error for a line that cannot be decoded as a request.
    InvalidLine(&'static str),
    /// A relevant filesystem change reported by the vault watcher.
    Watch(WatchMessage),
    /// End of standard input, which terminates the worker process.
    Eof,
}

/// Owns mutable worker state and coordinates request, watcher, and output channels.
///
/// Requests and watcher flushes are handled serially, so the contained
/// [`WorkerService`] never needs its own synchronization. Potentially blocking
/// service work is moved to Tokio's blocking pool and returned before the next
/// message is handled.
pub struct WorkerActor {
    service: WorkerService,
    /// Sender cloned into the vault watcher so changes re-enter this actor's queue.
    input: mpsc::Sender<ActorMessage>,
    /// Bounded response and event sink; awaiting delivery backpressures service work.
    /// The process boundary performs JSON serialization.
    output: mpsc::Sender<WorkerEnvelope>,
    /// Active watcher handle, retained to keep observation of the current vault alive.
    watcher: Option<VaultWatcher>,
    /// At most eight changed paths, or an empty-string sentinel requiring a full rescan.
    pending_paths: BTreeSet<String>,
    /// Deadline for publishing the next coalesced filesystem rescan.
    deadline: Option<Instant>,
}

impl WorkerActor {
    /// Create an actor that emits typed worker envelopes on `output`.
    pub fn new(input: mpsc::Sender<ActorMessage>, output: mpsc::Sender<WorkerEnvelope>) -> Self {
        Self {
            service: WorkerService::new(),
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
                Some(ActorMessage::InvalidLine(message)) => {
                    self.send_error(RequestId::INVALID, "invalid_request", message)
                        .await;
                }
                Some(ActorMessage::Watch(WatchMessage::Changed(paths))) => {
                    self.remember_watch_paths(paths);
                    self.deadline = Some(Instant::now() + Duration::from_millis(100));
                }
                Some(ActorMessage::Watch(WatchMessage::Error(error))) => {
                    self.service.record_watcher_error(error);
                }
                Some(ActorMessage::Eof) | None => break,
            }
        }
    }

    /// Coalesce paths without losing changes when the bounded notification list fills.
    /// A full-rescan sentinel remains authoritative until the debounce batch is flushed.
    fn remember_watch_paths(&mut self, paths: Vec<String>) {
        if self.pending_paths.contains("") {
            return;
        }
        for path in paths {
            if path.is_empty()
                || (self.pending_paths.len() == 8 && !self.pending_paths.contains(&path))
            {
                self.pending_paths.clear();
                self.pending_paths.insert(String::new());
                return;
            }
            self.pending_paths.insert(path);
        }
    }

    async fn handle_line(&mut self, line: &str) -> bool {
        let envelope = match protocol::decode(line) {
            Ok(envelope) => envelope,
            Err(message) => {
                self.send_error(RequestId::INVALID, "invalid_request", message)
                    .await;
                return true;
            }
        };
        let id = envelope.id;
        let starts_watcher = matches!(&envelope.request, Request::Initialize(_));
        let shutdown = matches!(&envelope.request, Request::Shutdown(_));
        let overlay_change = matches!(
            &envelope.request,
            Request::OverlayUpsert(_) | Request::OverlayCommit(_) | Request::OverlayRemove(_)
        );
        let result = self
            .blocking_service(move |service| {
                // Time and zone are sampled together inside the request operation so every
                // time-sensitive action performed by this dispatch shares one snapshot.
                let environment = ExecutionEnvironment::capture();
                service.handle(envelope.request, &environment)
            })
            .await;
        match result {
            Ok(handled) => {
                if starts_watcher {
                    self.start_watcher();
                }
                if let Some(change) = handled.index_change {
                    if overlay_change {
                        self.emit_index_changed(change, IndexChangeOrigin::Overlay)
                            .await;
                    }
                }
                let _ = self
                    .output
                    .send(WorkerEnvelope::Response(Box::new(
                        ResponseEnvelope::success(id, handled.success),
                    )))
                    .await;
            }
            Err(error) => {
                let _ = self
                    .output
                    .send(WorkerEnvelope::Response(Box::new(ResponseEnvelope::error(
                        id,
                        ErrorPayload {
                            code: error.code.to_owned(),
                            message: error.message,
                            available_views: error.available_views,
                        },
                    ))))
                    .await;
            }
        }
        !shutdown
    }

    async fn send_error(&self, id: RequestId, code: impl Into<String>, message: impl Into<String>) {
        let _ = self
            .output
            .send(WorkerEnvelope::Response(Box::new(ResponseEnvelope::error(
                id,
                ErrorPayload {
                    code: code.into(),
                    message: message.into(),
                    available_views: None,
                },
            ))))
            .await;
    }

    async fn emit_index_changed(&self, change: IndexChange, origin: IndexChangeOrigin) {
        let _ = self
            .output
            .send(WorkerEnvelope::Event(EventEnvelope {
                event: Event::IndexChanged {
                    generation: change.generation,
                    paths: change.paths,
                    origin,
                },
            }))
            .await;
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
        match self
            .blocking_service(move |service| service.reindex_external(paths))
            .await
        {
            Ok(change) => {
                self.emit_index_changed(change, IndexChangeOrigin::Watch)
                    .await;
            }
            Err(error) => self
                .service
                .record_watcher_error(format!("watcher rescan failed: {}", error.message)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tokio::time::timeout;

    fn actor() -> WorkerActor {
        let (input, _) = mpsc::channel(1);
        let (output, _) = mpsc::channel(1);
        WorkerActor::new(input, output)
    }

    #[test]
    fn overflow_requests_a_sticky_full_rescan() {
        let mut actor = actor();
        actor.remember_watch_paths((0..9).map(|i| format!("{i}.md")).collect());
        assert_eq!(actor.pending_paths, BTreeSet::from([String::new()]));
        actor.remember_watch_paths(vec!["later.md".into()]);
        assert_eq!(actor.pending_paths, BTreeSet::from([String::new()]));
    }

    #[test]
    fn duplicate_paths_do_not_overflow_a_full_notification_list() {
        let mut actor = actor();
        let paths = (0..8).map(|i| format!("{i}.md")).collect::<Vec<_>>();
        actor.remember_watch_paths(paths.clone());
        actor.remember_watch_paths(paths.clone());
        assert_eq!(actor.pending_paths, paths.into_iter().collect());
    }

    #[test]
    fn late_sentinel_overrides_a_full_notification_list() {
        let mut actor = actor();
        actor.remember_watch_paths((0..8).map(|i| format!("{i}.md")).collect());
        actor.remember_watch_paths(vec!["0.md".into(), String::new(), "later.md".into()]);
        assert_eq!(actor.pending_paths, BTreeSet::from([String::new()]));
    }

    fn line(id: u64, method: &str, params: Value) -> ActorMessage {
        ActorMessage::Line(
            json!({"id": id, "request": {"method": method, "params": params}}).to_string(),
        )
    }

    // Initialize the service directly so these tests do not depend on watcher timing.
    fn initialized_actor(root: &TempDir) -> (WorkerActor, mpsc::Receiver<WorkerEnvelope>) {
        let (input, _) = mpsc::channel(16);
        let (output, receiver) = mpsc::channel(1);
        let mut actor = WorkerActor::new(input, output);
        let request = protocol::decode(
            &json!({"id": 1, "request": {"method": "initialize", "params": {"vault_root": root.path()}}}).to_string(),
        ).unwrap();
        actor
            .service
            .handle(request.request, &ExecutionEnvironment::capture())
            .unwrap();
        (actor, receiver)
    }

    #[tokio::test]
    async fn stalled_output_backpressures_fetches_and_drains_in_order_before_shutdown() {
        let root = TempDir::new().unwrap();
        let payload = "x".repeat(66 * 1024);
        std::fs::write(
            root.path().join("Host.md"),
            format!("---\npayload: {payload}\n---\n"),
        )
        .unwrap();
        let (mut actor, mut output) = initialized_actor(&root);
        let request = protocol::decode(&json!({"id": 2, "request": {"method": "query", "params": {
            "source": {"kind": "inline", "text": "views:\n  - type: table\n    name: Table\n    order: [payload]\n"},
            "host_path": "Host.md", "preview_rows": 0
        }}}).to_string()).unwrap();
        let handled = actor
            .service
            .handle(request.request, &ExecutionEnvironment::capture())
            .unwrap();
        let crate::protocol::Success::Query(query) = handled.success else {
            panic!("expected query");
        };
        let (input, receiver) = mpsc::channel(16);
        for id in 3..7 {
            input
                .send(line(
                    id,
                    "fetch_rows",
                    json!({"result_id": query.result_id}),
                ))
                .await
                .unwrap();
        }
        input
            .send(ActorMessage::InvalidLine("request line is not UTF-8"))
            .await
            .unwrap();
        input
            .send(ActorMessage::Line("not json".to_owned()))
            .await
            .unwrap();
        input
            .send(line(7, "fetch_rows", json!({"result_id": "missing"})))
            .await
            .unwrap();
        input
            .send(line(
                8,
                "overlay_upsert",
                json!({"path": "Draft.md", "contents": "# Draft\n"}),
            ))
            .await
            .unwrap();
        input.send(line(9, "shutdown", json!({}))).await.unwrap();
        let mut task = tokio::spawn(actor.run(receiver));
        // With no consumer, the actor must stop at delivery rather than finish the pipeline.
        assert!(
            timeout(Duration::from_millis(100), &mut task)
                .await
                .is_err()
        );
        let mut envelopes = Vec::new();
        while let Some(envelope) = timeout(Duration::from_secs(5), output.recv())
            .await
            .unwrap()
        {
            envelopes.push(serde_json::to_value(envelope).unwrap());
        }
        task.await.unwrap();
        assert_eq!(envelopes.len(), 10);
        for (index, id) in (3..7).enumerate() {
            assert_eq!(envelopes[index]["id"], id);
            assert_eq!(
                envelopes[index]["response"]["result"]["data"]["rows"][0]["cells"][0]["text"],
                payload
            );
        }
        for envelope in &envelopes[4..6] {
            assert_eq!(envelope["id"], 0);
            assert_eq!(envelope["response"]["error"]["code"], "invalid_request");
        }
        assert_eq!(envelopes[6]["id"], 7);
        assert_eq!(envelopes[6]["response"]["error"]["code"], "unknown_result");
        assert_eq!(envelopes[7]["event"]["origin"], "overlay");
        assert_eq!(envelopes[8]["id"], 8);
        assert_eq!(envelopes[9]["id"], 9);
        assert_eq!(envelopes[9]["response"]["result"]["method"], "shutdown");
    }

    #[tokio::test]
    async fn watcher_event_waits_for_output_capacity() {
        let root = TempDir::new().unwrap();
        let (mut actor, mut output) = initialized_actor(&root);
        actor
            .send_error(RequestId::INVALID, "invalid_request", "seed")
            .await;
        actor.pending_paths.insert("Observed.md".to_owned());
        let flush = actor.flush_watch();
        tokio::pin!(flush);
        assert!(
            timeout(Duration::from_millis(100), &mut flush)
                .await
                .is_err()
        );
        assert!(matches!(
            output.recv().await.unwrap(),
            WorkerEnvelope::Response(_)
        ));
        timeout(Duration::from_secs(5), &mut flush).await.unwrap();
        let event = serde_json::to_value(output.recv().await.unwrap()).unwrap();
        assert_eq!(event["event"]["origin"], "watch");
        assert_eq!(event["event"]["generation"], 2);
        assert_eq!(event["event"]["paths"], json!(["Observed.md"]));
    }
}
