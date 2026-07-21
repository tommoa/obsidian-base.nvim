//! Filesystem watching and filtering for vault changes relevant to the index.

use std::path::Path;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// Change or error notification delivered from the filesystem watcher to the actor.
pub enum WatchMessage {
    /// Relevant vault-relative paths that should trigger a reindex.
    Changed(Vec<String>),
    /// Diagnostic emitted when the watcher backend reports a failure.
    Error(String),
}

/// Keeps a recursive `notify` watcher alive for the initialized vault.
pub struct VaultWatcher {
    /// Underlying watcher kept alive so its callback remains registered.
    _watcher: RecommendedWatcher,
}

impl VaultWatcher {
    pub fn start(
        root: &Path,
        sender: mpsc::Sender<crate::actor::ActorMessage>,
    ) -> notify::Result<Self> {
        let canonical_root = root.to_owned();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let message = match event {
                Ok(event) => relevant_paths(&canonical_root, event)
                    .filter(|paths| !paths.is_empty())
                    .map(WatchMessage::Changed),
                Err(error) => Some(WatchMessage::Error(error.to_string())),
            };
            if let Some(message) = message {
                let _ = sender.blocking_send(crate::actor::ActorMessage::Watch(message));
            }
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self { _watcher: watcher })
    }
}

fn relevant_paths(root: &Path, event: Event) -> Option<Vec<String>> {
    let needs_rescan = event.need_rescan();
    let mut paths = event
        .paths
        .into_iter()
        .filter_map(|path| relative(root, &path))
        .filter(|path| relevant(path))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    if paths.is_empty() && needs_rescan {
        // An empty path is the actor's sentinel for a full rescan when notify cannot name the
        // affected files (for example after an overflowed backend event queue).
        paths.push(String::new());
    }
    Some(paths)
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()?
        .to_str()
        .map(|path| path.replace('\\', "/"))
}

fn relevant(path: &str) -> bool {
    if path == ".obsidian/types.json" {
        return true;
    }
    !path
        .split('/')
        .any(|component| [".git", ".obsidian", ".trash"].contains(&component))
}
