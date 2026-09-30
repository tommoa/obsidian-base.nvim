use std::{collections::BTreeMap, fs, num::NonZeroUsize, path::Path};

use obsidian_base_worker::{
    ExecutionEnvironment, WorkerService,
    index::{Index, Overlay},
    limits::Limits,
    protocol::{
        FetchRowsParams, InitialiseParams, LimitsPatch, OverlayPathParams, OverlayUpsertParams,
        QueryParams, QuerySource,
    },
    value::DataValue,
};
use tempfile::TempDir;

fn write(root: &Path, path: &str, contents: &str) {
    let absolute = root.join(path);
    fs::create_dir_all(absolute.parent().unwrap()).unwrap();
    fs::write(absolute, contents).unwrap();
}

fn build(root: &Path, overlays: &BTreeMap<String, Overlay>) -> Index {
    Index::build(
        root.canonicalize().unwrap(),
        overlays,
        &Default::default(),
        Limits::default(),
        vec![],
    )
    .unwrap()
}

fn refresh(index: &Index, paths: &[&str], overlays: &BTreeMap<String, Overlay>) -> Index {
    index
        .refresh(
            &paths
                .iter()
                .map(|path| (*path).to_owned())
                .collect::<Vec<_>>(),
            overlays,
            &Default::default(),
            Limits::default(),
            vec![],
        )
        .unwrap()
}

/// Check the candidate against a fresh scan without mutating the old snapshot.
fn refresh_matches_scan(
    index: &Index,
    paths: &[&str],
    overlays: &BTreeMap<String, Overlay>,
) -> Index {
    let candidate = refresh(index, paths, overlays);
    assert_eq!(candidate.records, build(&index.root, overlays).records);
    candidate
}

#[test]
fn edits_creations_deletions_and_overlay_only_files_match_full_scans() {
    let root = TempDir::new().unwrap();
    let mut overlays = BTreeMap::new();
    write(root.path(), "Source.md", "#old [[Target]]\n");
    write(root.path(), "Target.md", "---\nrank: 1\n---\n");
    let original = build(root.path(), &overlays);
    write(root.path(), "Source.md", "#new [[Created]] [[Draft]]\n");
    write(root.path(), "Created.md", "---\nrank: 2\n---\n");
    fs::remove_file(root.path().join("Target.md")).unwrap();
    let edited = refresh_matches_scan(
        &original,
        &["Source.md", "Created.md", "Target.md"],
        &overlays,
    );
    assert_eq!(original.records["Source.md"].tags, ["#old"]);
    assert_eq!(edited.records["Created.md"].backlinks, ["Source.md"]);

    overlays.insert(
        "Draft.md".into(),
        Overlay {
            contents: "#draft [[Created]]\n".into(),
            created_at: 123,
        },
    );
    let draft = refresh_matches_scan(&edited, &["Draft.md"], &overlays);
    assert_eq!(draft.records["Draft.md"].ctime, 123);
    assert_eq!(draft.records["Draft.md"].backlinks, ["Source.md"]);
    overlays.clear();
    let removed = refresh_matches_scan(&draft, &["Draft.md"], &overlays);
    assert!(!removed.records.contains_key("Draft.md"));
}

#[test]
fn target_changes_recompute_natural_winners_relative_precedence_and_duplicate_order() {
    let root = TempDir::new().unwrap();
    let overlays = BTreeMap::new();
    write(root.path(), "a10/Target.md", "# Target\n");
    write(root.path(), "z/Source.md", "[[Target]] [[Target]]\n");
    write(root.path(), "a/Source.md", "[[Target]]\n");
    let original = build(root.path(), &overlays);
    assert_eq!(
        original.records["a10/Target.md"].backlinks,
        ["a/Source.md", "z/Source.md", "z/Source.md"]
    );
    write(root.path(), "a2/Target.md", "# Earlier natural winner\n");
    let winner = refresh_matches_scan(&original, &["a2/Target.md"], &overlays);
    assert_eq!(
        winner.resolve_link("Target", "z/Source.md").as_deref(),
        Some("a2/Target.md")
    );
    assert!(winner.records["a10/Target.md"].backlinks.is_empty());
    write(root.path(), "z/Target.md", "# Relative winner\n");
    let relative = refresh_matches_scan(&winner, &["z/Target.md"], &overlays);
    assert_eq!(
        relative.records["z/Target.md"].backlinks,
        ["z/Source.md", "z/Source.md"]
    );
    assert_eq!(relative.records["a2/Target.md"].backlinks, ["a/Source.md"]);
    fs::remove_file(root.path().join("z/Target.md")).unwrap();
    fs::remove_file(root.path().join("a2/Target.md")).unwrap();
    let restored = refresh_matches_scan(&relative, &["z/Target.md", "a2/Target.md"], &overlays);
    assert_eq!(
        restored.records["a10/Target.md"].backlinks,
        original.records["a10/Target.md"].backlinks
    );
}

#[test]
fn named_hints_reuse_unrelated_snapshot_records_and_ambiguous_hints_scan() {
    let root = TempDir::new().unwrap();
    let overlays = BTreeMap::new();
    write(root.path(), "Named.md", "# Named\n");
    write(root.path(), "Unrelated.md", "---\nrank: 1\n---\n");
    let original = build(root.path(), &overlays);
    write(root.path(), "Named.md", "# Changed\n");
    write(root.path(), "Unrelated.md", "---\nrank: 2\n---\n");
    // A named hint represents a complete change set: unmentioned disk changes
    // intentionally remain absent from this published snapshot until a full scan.
    let named = refresh(&original, &["Named.md"], &overlays);
    assert_eq!(
        named.records["Unrelated.md"],
        original.records["Unrelated.md"]
    );
    for hints in [vec![], vec![""], vec!["unknown.txt"]] {
        let scanned = refresh_matches_scan(&named, &hints, &overlays);
        assert_eq!(
            scanned.records["Unrelated.md"].properties["rank"],
            DataValue::Number(2.0)
        );
    }
}

#[test]
fn directory_hints_include_new_descendants_and_remove_supported_extension_subtrees() {
    let root = TempDir::new().unwrap();
    let overlays = BTreeMap::new();
    write(root.path(), "folder/Old.md", "# Old\n");
    write(root.path(), "archive.md/Child.md", "# Child\n");
    let original = build(root.path(), &overlays);
    write(root.path(), "folder/New.md", "# New\n");
    let added = refresh_matches_scan(&original, &["folder"], &overlays);
    assert!(added.records.contains_key("folder/New.md"));
    fs::remove_dir_all(root.path().join("archive.md")).unwrap();
    let removed = refresh_matches_scan(&added, &["archive.md"], &overlays);
    assert!(!removed.records.contains_key("archive.md/Child.md"));
}

#[test]
fn metadata_schema_changes_recoerce_unmentioned_notes() {
    let root = TempDir::new().unwrap();
    let overlays = BTreeMap::new();
    write(root.path(), ".obsidian/types.json", "{\"types\":{}}");
    write(root.path(), "Named.md", "# Named\n");
    write(
        root.path(),
        "Unrelated.md",
        "---\nwhen: '2026-07-24'\n---\n",
    );
    let original = build(root.path(), &overlays);
    assert!(matches!(
        original.records["Unrelated.md"].properties["when"],
        DataValue::String(_)
    ));
    write(
        root.path(),
        ".obsidian/types.json",
        "{\"types\":{\"when\":\"date\"}}",
    );
    let typed = refresh_matches_scan(&original, &["Named.md"], &overlays);
    assert!(matches!(
        typed.records["Unrelated.md"].properties["when"],
        DataValue::Date(_)
    ));
    write(root.path(), ".obsidian/types.json", "{\"types\":{}}");
    let untyped = refresh_matches_scan(&typed, &[".obsidian/types.json"], &overlays);
    assert!(matches!(
        untyped.records["Unrelated.md"].properties["when"],
        DataValue::String(_)
    ));
}

#[cfg(unix)]
#[test]
fn file_and_parent_symlinks_are_not_incrementally_indexed() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().unwrap();
    let overlays = BTreeMap::new();
    write(root.path(), "real/Note.md", "# Note\n");
    let original = build(root.path(), &overlays);
    symlink(
        root.path().join("real/Note.md"),
        root.path().join("Alias.md"),
    )
    .unwrap();
    symlink(root.path().join("real"), root.path().join("alias-folder")).unwrap();
    let refreshed =
        refresh_matches_scan(&original, &["Alias.md", "alias-folder/Note.md"], &overlays);
    assert_eq!(refreshed.records.len(), 1);
    assert!(!refreshed.records.contains_key("Alias.md"));
    assert!(!refreshed.records.contains_key("alias-folder/Note.md"));
}

#[test]
fn failed_multi_path_refresh_and_overlay_removal_preserve_published_service_state() {
    let root = TempDir::new().unwrap();
    write(root.path(), "A.md", "---\nrank: 1\n---\n");
    write(root.path(), "B.md", "# B\n");
    let environment = ExecutionEnvironment::new(
        "2026-07-24T00:00:00Z".parse().unwrap(),
        jiff::tz::TimeZone::UTC,
    );
    let mut service = WorkerService::new();
    service
        .initialize(InitialiseParams {
            vault_root: root.path().to_string_lossy().into_owned(),
            metadata_overrides: BTreeMap::new(),
            limits: LimitsPatch {
                source_bytes: Some(NonZeroUsize::new(512).unwrap()),
                ..LimitsPatch::default()
            },
        })
        .unwrap();
    service
        .overlay_upsert(
            OverlayUpsertParams {
                path: "A.md".into(),
                contents: "---\nrank: 3\n---\n".into(),
            },
            &environment,
        )
        .unwrap();
    let query_params = || QueryParams {
        source: QuerySource::Inline {
            text: "views:\n  - type: table\n    name: Table\n    order: [rank]\n".into(),
            source_id: None,
        },
        host_path: "A.md".into(),
        view_name: Some("Table".into()),
        preview_rows: 10,
    };
    let result = service.query(query_params(), &environment).unwrap();
    let before = serde_json::to_value(service.inspect()).unwrap();
    let rows = serde_json::to_value(
        service
            .fetch_rows(FetchRowsParams {
                result_id: result.result_id.clone(),
            })
            .unwrap(),
    )
    .unwrap();
    // The candidate successfully inserts C before B fails; neither partial
    // records nor result-cache invalidation may escape the failed transaction.
    write(root.path(), "C.md", "# New\n");
    write(root.path(), "B.md", &"x".repeat(513));
    assert_eq!(
        service
            .reindex_external(vec!["C.md".into(), "B.md".into()])
            .err()
            .unwrap()
            .code,
        "input_too_large"
    );
    assert_eq!(serde_json::to_value(service.inspect()).unwrap(), before);
    // A replacement overlay must also roll back when the pending full scan
    // encounters the still-invalid B file.
    assert_eq!(
        service
            .overlay_upsert(
                OverlayUpsertParams {
                    path: "A.md".into(),
                    contents: "---\nrank: 9\n---\n".into(),
                },
                &environment,
            )
            .err()
            .unwrap()
            .code,
        "input_too_large"
    );
    assert_eq!(serde_json::to_value(service.inspect()).unwrap(), before);
    assert_eq!(
        serde_json::to_value(
            service
                .fetch_rows(FetchRowsParams {
                    result_id: result.result_id.clone()
                })
                .unwrap()
        )
        .unwrap(),
        rows
    );
    write(root.path(), "A.md", &"x".repeat(513));
    assert_eq!(
        service
            .overlay_remove(OverlayPathParams {
                path: "A.md".into()
            })
            .err()
            .unwrap()
            .code,
        "input_too_large"
    );
    assert_eq!(serde_json::to_value(service.inspect()).unwrap(), before);
    assert_eq!(
        serde_json::to_value(
            service
                .fetch_rows(FetchRowsParams {
                    result_id: result.result_id.clone()
                })
                .unwrap()
        )
        .unwrap(),
        rows
    );
    // Fresh evaluation must still use the old published snapshot, including
    // the original overlay value and the absence of the partially inserted C.
    let still_published = service.query(query_params(), &environment).unwrap();
    assert_eq!(still_published.matched_count, 2);
    assert_eq!(
        serde_json::to_value(still_published.preview_rows).unwrap(),
        serde_json::to_value(&result.preview_rows).unwrap()
    );
    write(root.path(), "A.md", "---\nrank: 4\n---\n");
    write(root.path(), "B.md", "# Fixed\n");
    service
        .overlay_remove(OverlayPathParams {
            path: "A.md".into(),
        })
        .unwrap();
    assert!(service.inspect().overlays.is_empty());
    assert_eq!(
        service.inspect().generation,
        before["generation"].as_u64().unwrap() + 1
    );
    assert_eq!(
        service
            .fetch_rows(FetchRowsParams {
                result_id: result.result_id
            })
            .unwrap_err()
            .code,
        "unknown_result"
    );
    // The failed watcher batch was consumed, so a later update naming only A
    // must recover C too rather than permanently dropping the earlier hint.
    let recovered = service.query(query_params(), &environment).unwrap();
    assert_eq!(recovered.matched_count, 3);
    assert!(recovered.preview_rows.iter().any(|row| row.path == "C.md"));
}
