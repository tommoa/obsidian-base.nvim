use std::{
    collections::BTreeMap,
    fs,
    time::{Duration, Instant},
};

use obsidian_base_worker::{
    WorkerService,
    protocol::{InitializeParams, LimitsPatch, QueryParams, QuerySource},
};
use tempfile::TempDir;

#[test]
fn indexes_and_queries_five_thousand_files_within_thirty_seconds() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("Host.md"), "# Host\n").unwrap();
    let started = Instant::now();
    for index in 0..5_000 {
        fs::write(
            root.path().join(format!("note-{index:04}.md")),
            format!("---\nrank: {index}\n---\n# Note {index}\n"),
        )
        .unwrap();
    }
    let mut service = WorkerService::new();
    service
        .initialize(InitializeParams {
            vault_root: root.path().to_string_lossy().into_owned(),
            metadata_overrides: BTreeMap::new(),
            limits: LimitsPatch {
                query_ms: Some(10_000),
                result_rows: Some(10_000),
                ..LimitsPatch::default()
            },
        })
        .unwrap();
    let result = service
        .query(QueryParams {
            source: QuerySource::Inline {
                text: "views:\n  - type: table\n    name: Table\n    order: []\n".to_owned(),
                source_id: None,
            },
            host_path: "Host.md".to_owned(),
            view_name: Some("Table".to_owned()),
            preview_rows: 1,
        })
        .unwrap();
    assert_eq!(result.matched_count, 5_001);
    assert_eq!(result.view_count, 5_001);
    assert!(started.elapsed() < Duration::from_secs(30));
}
