//! Filesystem watching and filtering for vault changes relevant to the index.

use std::path::Path;

use notify::{
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind},
};
use tokio::sync::mpsc;

/// Change or error notification delivered from the filesystem watcher to the actor.
pub enum WatchMessage {
    /// Relevant vault-relative paths, or an empty-string sentinel requiring a full rescan.
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
    // Backend overflow is authoritative even when the event includes a few named paths.
    // The empty path tells the index that unchanged records cannot safely be reused.
    if needs_rescan {
        return Some(vec![String::new()]);
    }
    if matches!(event.kind, EventKind::Access(_)) {
        return None;
    }
    let precise = matches!(
        event.kind,
        EventKind::Create(CreateKind::File)
            | EventKind::Remove(RemoveKind::File)
            | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_))
    );
    let mut uncertain = event.paths.is_empty();
    let mut paths = Vec::new();
    for path in event.paths {
        match relative(root, &path) {
            Some(path) if relevant(&path) => paths.push(path),
            // A directory-level event can add/remove the otherwise included types registry.
            Some(path) if !precise && path == ".obsidian" => uncertain = true,
            Some(_) => (),
            None => uncertain = true,
        }
    }
    if paths.is_empty() && !uncertain {
        return None;
    }
    // Directories, renames, and backend-specific kinds may affect descendants or omit
    // a rename endpoint. Unnamed/non-UTF-8 paths likewise cannot identify reusable records.
    if uncertain || !precise {
        return Some(vec![String::new()]);
    }
    paths.sort();
    paths.dedup();
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

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, DataChange, Flag, MetadataKind, RenameMode};

    fn named(kind: EventKind, path: &str) -> Event {
        Event::new(kind).add_path(Path::new("/vault").join(path))
    }

    fn filtered(event: Event) -> Option<Vec<String>> {
        relevant_paths(Path::new("/vault"), event)
    }

    #[test]
    fn rescan_flag_overrides_named_and_excluded_paths() {
        for path in ["Task.md", ".git/index"] {
            assert_eq!(
                filtered(
                    named(EventKind::Modify(ModifyKind::Data(DataChange::Any)), path)
                        .set_flag(Flag::Rescan)
                ),
                Some(vec![String::new()])
            );
        }
    }

    #[test]
    fn precise_file_events_keep_sorted_unique_paths() {
        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Remove(RemoveKind::File),
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
        ] {
            let event = named(kind, "B.md")
                .add_path(Path::new("/vault/A.md").to_owned())
                .add_path(Path::new("/vault/B.md").to_owned());
            assert_eq!(filtered(event), Some(vec!["A.md".into(), "B.md".into()]));
        }
    }

    #[test]
    fn ambiguous_directory_and_rename_events_require_full_rescan() {
        for kind in [
            EventKind::Any,
            EventKind::Other,
            EventKind::Create(CreateKind::Folder),
            EventKind::Remove(RemoveKind::Folder),
            EventKind::Create(CreateKind::Any),
            EventKind::Modify(ModifyKind::Any),
            EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        ] {
            assert_eq!(filtered(named(kind, "Notes")), Some(vec![String::new()]));
            assert_eq!(
                filtered(named(kind, ".obsidian")),
                Some(vec![String::new()])
            );
        }
    }

    #[test]
    fn unnamed_and_unrepresentable_changes_require_full_rescan() {
        let kind = EventKind::Modify(ModifyKind::Data(DataChange::Any));
        assert_eq!(filtered(Event::new(kind)), Some(vec![String::new()]));
        assert_eq!(
            filtered(Event::new(kind).add_path(Path::new("/outside/Task.md").to_owned())),
            Some(vec![String::new()])
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_require_full_rescan() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let path = Path::new("/vault").join(OsString::from_vec(vec![0xff]));
        assert_eq!(
            filtered(Event::new(EventKind::Create(CreateKind::File)).add_path(path)),
            Some(vec![String::new()])
        );
    }

    #[test]
    fn access_and_ordinary_excluded_events_are_ignored() {
        assert_eq!(
            filtered(Event::new(EventKind::Access(AccessKind::Any))),
            None
        );
        assert_eq!(
            filtered(named(EventKind::Access(AccessKind::Any), "Task.md")),
            None
        );
        for kind in [EventKind::Any, EventKind::Create(CreateKind::File)] {
            for path in [".git/index", ".obsidian/workspace.json", ".trash/Task.md"] {
                assert_eq!(filtered(named(kind, path)), None);
            }
        }
        assert_eq!(
            filtered(named(
                EventKind::Modify(ModifyKind::Data(DataChange::Any)),
                ".obsidian/types.json"
            )),
            Some(vec![".obsidian/types.json".into()])
        );
    }
}
