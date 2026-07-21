use std::{
    collections::BTreeMap,
    fs,
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
};

use obsidian_base_worker::{
    WorkerService,
    protocol::{
        FetchRowsParams, InitializeParams, LimitsPatch, OverlayPathParams, OverlayUpsertParams,
        QueryParams, QuerySource,
    },
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

fn initialized() -> WorkerService {
    let mut service = WorkerService::new();
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fixtures().join("manifest.json")).unwrap())
            .unwrap();
    let metadata_overrides = serde_json::from_value(manifest["metadata"].clone()).unwrap();
    service
        .initialize(InitializeParams {
            vault_root: fixtures().join("vault").to_string_lossy().into_owned(),
            metadata_overrides,
            limits: LimitsPatch::default(),
        })
        .unwrap();
    service
}

fn inline_base(ordinal: usize) -> String {
    fs::read_to_string(fixtures().join("vault/Calendar/Daily/2026-07-15.md"))
        .unwrap()
        .split("```base\n")
        .nth(ordinal)
        .unwrap()
        .split("```")
        .next()
        .unwrap()
        .to_owned()
}

fn inline_query(text: &str, view: &str) -> QueryParams {
    inline_query_at(text, view, "Calendar/Daily/2026-07-15.md")
}

fn inline_query_at(text: &str, view: &str, host_path: &str) -> QueryParams {
    QueryParams {
        source: QuerySource::Inline {
            text: text.to_owned(),
            source_id: Some("fixture".to_owned()),
        },
        host_path: host_path.to_owned(),
        view_name: Some(view.to_owned()),
        preview_rows: 20,
    }
}

fn paths(result: &obsidian_base_worker::protocol::QueryResult) -> Vec<&str> {
    result
        .preview_rows
        .iter()
        .map(|row| row.path.as_str())
        .collect()
}

#[test]
fn fixture_views_match_the_reference_results() {
    let mut service = initialized();
    let personal = service
        .query(inline_query(&inline_base(2), "Personal"))
        .unwrap();
    assert_eq!(
        paths(&personal),
        ["Encounters/general note.md", "Activities/Clean kitchen.md"]
    );
    assert_eq!(
        personal
            .available_views
            .iter()
            .map(|view| view.name.as_str())
            .collect::<Vec<_>>(),
        ["Personal", "Work"]
    );
    let activity = service
        .query(inline_query(&inline_base(1), "Activity tracker"))
        .unwrap();
    assert_eq!(
        paths(&activity),
        ["Activities/Clean kitchen.md", "Activities/Review work.md"]
    );
    assert_eq!(activity.preview_rows[0].cells[2].kind, "link");
    let file = service
        .query(QueryParams {
            source: QuerySource::File {
                path: "Bases/Daily log tasks.base".to_owned(),
                source_id: None,
            },
            host_path: "Calendar/Daily/2026-07-15.md".to_owned(),
            view_name: Some("Work".to_owned()),
            preview_rows: 20,
        })
        .unwrap();
    assert_eq!(
        paths(&file),
        ["Encounters/work note.md", "Activities/Review work.md"]
    );
}

#[test]
fn view_catalog_keeps_named_table_views_once_in_source_order() {
    let mut service = initialized();
    let source = "views:\n  - type: list\n    name: Unsupported\n  - type: table\n    name: Grouped\n    groupBy: property.group\n    order: []\n  - type: table\n    name: First\n    order: []\n  - type: table\n    name: First\n    order: []\n  - type: table\n    name: Second\n    order: []\n";
    let result = service
        .query(QueryParams {
            source: QuerySource::Inline {
                text: source.to_owned(),
                source_id: Some("fixture".to_owned()),
            },
            host_path: "Calendar/Daily/2026-07-15.md".to_owned(),
            view_name: None,
            preview_rows: 20,
        })
        .unwrap();
    assert_eq!(result.view.name, "First");
    assert_eq!(
        result
            .available_views
            .iter()
            .map(|view| view.name.as_str())
            .collect::<Vec<_>>(),
        ["First", "Second"]
    );
}

#[test]
fn unnamed_table_view_is_renderable_but_not_selectable() {
    let mut service = initialized();
    let source =
        "views:\n  - type: table\n    order: []\n  - type: table\n    name: Named\n    order: []\n";
    let result = service
        .query(QueryParams {
            source: QuerySource::Inline {
                text: source.to_owned(),
                source_id: Some("fixture".to_owned()),
            },
            host_path: "Calendar/Daily/2026-07-15.md".to_owned(),
            view_name: None,
            preview_rows: 20,
        })
        .unwrap();
    assert_eq!(result.view.name, "table");
    assert_eq!(
        result
            .available_views
            .iter()
            .map(|view| view.name.as_str())
            .collect::<Vec<_>>(),
        ["Named"]
    );
}

#[test]
fn empty_table_view_name_is_normalized_as_unnamed() {
    let mut service = initialized();
    let source = "views:\n  - type: table\n    name: \"\"\n    order: []\n  - type: table\n    name: Named\n    order: []\n";
    let result = service
        .query(QueryParams {
            source: QuerySource::Inline {
                text: source.to_owned(),
                source_id: Some("fixture".to_owned()),
            },
            host_path: "Calendar/Daily/2026-07-15.md".to_owned(),
            view_name: None,
            preview_rows: 20,
        })
        .unwrap();
    assert_eq!(result.view.name, "table");
    assert_eq!(
        result
            .available_views
            .iter()
            .map(|view| view.name.as_str())
            .collect::<Vec<_>>(),
        ["Named"]
    );
    assert_eq!(
        service.query(inline_query(source, "")).unwrap_err().code,
        "unknown_view"
    );
}

#[test]
fn query_errors_preserve_the_current_view_catalog() {
    let mut service = initialized();
    let source = "views:\n  - type: table\n    name: Current\n    order: []\n  - type: table\n    name: Other\n    order: []\n";
    let error = service.query(inline_query(source, "Removed")).unwrap_err();
    assert_eq!(error.code, "unknown_view");
    assert_eq!(
        error
            .available_views
            .unwrap()
            .iter()
            .map(|view| view.name.as_str())
            .collect::<Vec<_>>(),
        ["Current", "Other"]
    );
}

#[test]
fn oversized_query_error_catalog_is_omitted() {
    let mut service = WorkerService::new();
    service
        .initialize(InitializeParams {
            vault_root: fixtures().join("vault").to_string_lossy().into_owned(),
            metadata_overrides: BTreeMap::new(),
            limits: LimitsPatch {
                result_bytes: Some(NonZeroUsize::new(100).unwrap()),
                ..LimitsPatch::default()
            },
        })
        .unwrap();
    let views = (0..100)
        .map(|index| format!("  - type: table\n    name: View {index}\n    order: []\n"))
        .collect::<String>();
    let error = service
        .query(inline_query(&format!("views:\n{views}"), "Removed"))
        .unwrap_err();
    assert_eq!(error.code, "unknown_view");
    assert!(error.available_views.is_none());
}

#[test]
fn overlays_emit_and_invalidate_cached_results() {
    let mut service = initialized();
    let before = service
        .query(inline_query(&inline_base(2), "Work"))
        .unwrap();
    let result_id = before.result_id.clone();
    assert!(
        service
            .fetch_rows(FetchRowsParams {
                result_id: result_id.clone()
            })
            .is_ok()
    );
    let change = service
        .overlay_upsert(OverlayUpsertParams {
            path: "Activities/Clean kitchen.md".to_owned(),
            contents: fs::read_to_string(fixtures().join("overlays/Clean kitchen.work.md"))
                .unwrap(),
        })
        .unwrap();
    assert_eq!(change.generation, 2);
    assert_eq!(change.paths, ["Activities/Clean kitchen.md"]);
    assert_eq!(
        service
            .fetch_rows(FetchRowsParams { result_id })
            .unwrap_err()
            .code,
        "unknown_result"
    );
    let work = service
        .query(inline_query(&inline_base(2), "Work"))
        .unwrap();
    assert!(paths(&work).contains(&"Activities/Clean kitchen.md"));
}

#[test]
fn overlay_only_host_and_formula_cache_work() {
    let mut service = initialized();
    service
        .overlay_upsert(OverlayUpsertParams {
            path: "Draft.md".to_owned(),
            contents: "---\nlabels: []\n---\n# Draft\n".to_owned(),
        })
        .unwrap();
    let source = "filters:\n  - file.name == \"Draft\"\n  - property.labels.isEmpty()\nformulas:\n  self: file.asLink()\nviews:\n  - type: table\n    name: Table\n    order: [formula.self, formula.self]\n";
    let result = service
        .query(inline_query_at(source, "Table", "Draft.md"))
        .unwrap();
    assert_eq!(paths(&result), ["Draft.md"]);
    assert_eq!(result.preview_rows[0].cells[0].text, "Draft");
    service
        .overlay_remove(OverlayPathParams {
            path: "Draft.md".to_owned(),
        })
        .unwrap();
    assert_eq!(
        service
            .query(inline_query_at(source, "Table", "Draft.md"))
            .unwrap_err()
            .code,
        "missing_host"
    );
}

#[test]
fn query_wide_limits_and_result_bytes_are_enforced() {
    let mut service = WorkerService::new();
    service
        .initialize(InitializeParams {
            vault_root: fixtures().join("vault").to_string_lossy().into_owned(),
            metadata_overrides: BTreeMap::new(),
            limits: LimitsPatch {
                evaluation_steps: Some(NonZeroU64::new(4).unwrap()),
                result_bytes: Some(NonZeroUsize::new(100_000).unwrap()),
                ..LimitsPatch::default()
            },
        })
        .unwrap();
    let source = "filters: file.name == \"never\"\nviews:\n  - type: table\n    name: Table\n    order: []\n";
    assert_eq!(
        service
            .query(inline_query(source, "Table"))
            .unwrap_err()
            .code,
        "evaluation_limit"
    );

    let mut service = initialized();
    let invalid = "views:\n  - type: table\n    name: Table\n    limit: -1\n    order: []\n";
    assert_eq!(
        service
            .query(inline_query(invalid, "Table"))
            .unwrap_err()
            .code,
        "invalid_view"
    );
}

#[test]
fn result_ids_follow_the_query_allocation_boundary() {
    let mut service = initialized();
    let source = inline_base(2);
    let mut invalid = inline_query(&source, "Missing");
    invalid.source = QuerySource::Inline {
        text: source,
        source_id: Some("invalid".to_owned()),
    };
    assert_eq!(service.query(invalid).unwrap_err().code, "unknown_view");
    assert_eq!(
        service
            .query(inline_query(&inline_base(2), "Personal"))
            .unwrap()
            .result_id,
        "r1-1"
    );

    let mut service = initialized();
    let cycle = "formulas:\n  a: formula.b\n  b: formula.a\nviews:\n  - type: table\n    name: Table\n    order: [formula.a]\n";
    let mut cyclic = inline_query(cycle, "Table");
    cyclic.source = QuerySource::Inline {
        text: cycle.to_owned(),
        source_id: Some("cycle".to_owned()),
    };
    assert_eq!(service.query(cyclic).unwrap_err().code, "formula_cycle");
    assert_eq!(
        service
            .query(inline_query(&inline_base(2), "Personal"))
            .unwrap()
            .result_id,
        "r1-2"
    );
}
