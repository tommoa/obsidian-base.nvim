use std::{collections::BTreeMap, fs, path::Path};

use obsidian_base_worker::{
    WorkerService,
    protocol::{FetchRowsParams, InitializeParams, LimitsPatch, QueryParams, QuerySource},
};
use tempfile::TempDir;

fn host(root: &Path) {
    fs::create_dir_all(root.join(".obsidian")).unwrap();
    fs::write(root.join("Host.md"), "# Host\n").unwrap();
}

fn initialize(service: &mut WorkerService, root: &Path) {
    service
        .initialize(InitializeParams {
            vault_root: root.to_string_lossy().into_owned(),
            metadata_overrides: BTreeMap::new(),
            limits: LimitsPatch::default(),
        })
        .unwrap();
}

fn inline_query(text: &str) -> QueryParams {
    QueryParams {
        source: QuerySource::Inline {
            text: text.to_owned(),
            source_id: None,
        },
        host_path: "Host.md".to_owned(),
        view_name: Some("Table".to_owned()),
        preview_rows: 50,
    }
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_sources_outside_the_vault() {
    use std::os::unix::fs::symlink;

    let parent = TempDir::new().unwrap();
    let root = parent.path().join("vault");
    host(&root);
    let outside = parent.path().join("outside.base");
    fs::write(&outside, "views: []\n").unwrap();
    symlink(&outside, root.join("Escaped.base")).unwrap();
    let mut service = WorkerService::new();
    initialize(&mut service, &root);
    let error = service
        .query(QueryParams {
            source: QuerySource::File {
                path: "Escaped.base".to_owned(),
                source_id: None,
            },
            host_path: "Host.md".to_owned(),
            view_name: None,
            preview_rows: 50,
        })
        .unwrap_err();
    assert_eq!(error.code, "path_outside_vault");
}

#[test]
fn failed_rescan_preserves_generation_and_cached_rows() {
    let root = TempDir::new().unwrap();
    host(root.path());
    fs::write(root.path().join(".obsidian/types.json"), "{\"types\":{}}").unwrap();
    let mut service = WorkerService::new();
    initialize(&mut service, root.path());
    let result = service
        .query(inline_query(
            "views:\n  - type: table\n    name: Table\n    order: []\n",
        ))
        .unwrap();
    let id = result.result_id;
    let before = serde_json::to_value(service.inspect()).unwrap();
    fs::write(root.path().join(".obsidian/types.json"), "{").unwrap();
    assert!(
        service
            .reindex_external(vec![".obsidian/types.json".into()])
            .is_err()
    );
    assert_eq!(serde_json::to_value(service.inspect()).unwrap(), before);
    assert!(
        service
            .fetch_rows(FetchRowsParams { result_id: id })
            .is_ok()
    );
}

#[test]
fn rejects_unsafe_yaml_and_deep_filter_trees() {
    let root = TempDir::new().unwrap();
    host(root.path());
    let mut service = WorkerService::new();
    initialize(&mut service, root.path());
    let unsafe_source = "filters: &filter file.name\nviews:\n  - type: table\n    filters: *filter\n    order: []\n";
    assert_eq!(
        service.query(inline_query(unsafe_source)).unwrap_err().code,
        "unsafe_yaml"
    );
}

#[test]
fn rejects_vault_text_larger_than_the_configured_limit() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Large.md"), "x".repeat(65)).unwrap();
    let mut service = WorkerService::new();
    let error = service
        .initialize(InitializeParams {
            vault_root: root.path().to_string_lossy().into_owned(),
            metadata_overrides: BTreeMap::new(),
            limits: LimitsPatch {
                source_bytes: Some(64),
                ..LimitsPatch::default()
            },
        })
        .unwrap_err();
    assert_eq!(error.code, "input_too_large");
}

#[test]
fn rejects_existing_overlays_when_the_limit_is_lowered() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Host.md"), "# Host\n").unwrap();
    let mut service = WorkerService::new();
    service
        .initialize(InitializeParams {
            vault_root: root.path().to_string_lossy().into_owned(),
            metadata_overrides: BTreeMap::new(),
            limits: LimitsPatch {
                source_bytes: Some(128),
                ..LimitsPatch::default()
            },
        })
        .unwrap();
    service
        .overlay_upsert(obsidian_base_worker::protocol::OverlayUpsertParams {
            path: "Draft.md".to_owned(),
            contents: "x".repeat(65),
        })
        .unwrap();
    let error = service
        .initialize(InitializeParams {
            vault_root: root.path().to_string_lossy().into_owned(),
            metadata_overrides: BTreeMap::new(),
            limits: LimitsPatch {
                source_bytes: Some(64),
                ..LimitsPatch::default()
            },
        })
        .unwrap_err();
    assert_eq!(error.code, "input_too_large");
}

#[test]
fn switching_vaults_discards_root_scoped_state() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    host(first.path());
    host(second.path());
    let mut service = WorkerService::new();
    initialize(&mut service, first.path());
    service
        .overlay_upsert(obsidian_base_worker::protocol::OverlayUpsertParams {
            path: "Draft.md".to_owned(),
            contents: "# Unsaved draft\n".to_owned(),
        })
        .unwrap();
    service.record_watcher_error("first vault error".to_owned());

    initialize(&mut service, second.path());

    let inspection = service.inspect();
    assert!(inspection.overlays.is_empty());
    assert!(inspection.watcher_errors.is_empty());
}
