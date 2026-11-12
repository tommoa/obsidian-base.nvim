use std::{
    collections::BTreeMap,
    fs,
    num::{NonZeroU64, NonZeroUsize},
    time::{Duration, Instant},
};

use obsidian_base_worker::{
    ExecutionEnvironment, WorkerService,
    index::{Index, Overlay},
    limits::Limits,
    protocol::{
        InitialiseParams, LimitsPatch, OverlayPathParams, OverlayUpsertParams, QueryParams,
        QuerySource,
    },
    query::{QueryInput, execute},
};
use tempfile::TempDir;

fn environment() -> ExecutionEnvironment {
    ExecutionEnvironment::new(
        "2026-07-24T00:00:00Z".parse().unwrap(),
        jiff::tz::TimeZone::UTC,
    )
}

/// Query-only probe: indexing, filesystem I/O, and result-cache eviction are
/// outside the timing. Run in release mode with `--ignored --nocapture
/// --test-threads=1`. Each sample averages five complete executions, including
/// parsing, filtering, sorting, rendering, and result-byte validation.
/// Set `OBSIDIAN_BASE_BENCH_WORKLOAD` to natural, numeric, selective_formula, or
/// date to isolate one query shape for process-level memory measurements.
#[test]
#[ignore = "manual release-mode benchmark; prints timings without a machine-specific threshold"]
fn benchmark_query_workloads() {
    const FILES: usize = 5_000;
    const SAMPLES: usize = 9;
    const BATCH: usize = 5;
    let workload = std::env::var("OBSIDIAN_BASE_BENCH_WORKLOAD").ok();
    if let Some(workload) = workload.as_deref() {
        assert!(
            ["natural", "numeric", "selective_formula", "date"].contains(&workload),
            "unknown benchmark workload: {workload}",
        );
    }
    let root = TempDir::new().unwrap();
    let overlays = (0..FILES)
        .map(|ordinal| {
            (
                format!("folder-{}/note-{ordinal}.md", ordinal % 20),
                Overlay {
                    contents: format!(
                        "---\nrank: {}\nstatus: active\ndate: \"2026-07-{:02}\"\ntags: [bench]\n---\n",
                        (ordinal * 7919) % FILES,
                        ordinal % 28 + 1,
                    ),
                    created_at: 0,
                },
            )
        })
        .collect();
    let limits = Limits {
        evaluation_ms: 10_000,
        result_rows: 10_000,
        ..Limits::default()
    };
    let index = Index::build(
        root.path().to_owned(),
        &overlays,
        &Default::default(),
        limits,
        Vec::new(),
    )
    .unwrap();
    let environment = environment();
    for (label, filters, formulas, sort, count) in [
        ("natural", "", "", "", FILES),
        (
            "numeric",
            "filters: file.hasTag(\"#bench\")\n",
            "",
            "    sort:\n      - property: rank\n        direction: ASC\n",
            FILES,
        ),
        (
            "selective_formula",
            "filters: formula.score < 500\n",
            "formulas:\n  score: rank + 1\n",
            "    sort:\n      - property: formula.score\n        direction: DESC\n",
            499,
        ),
        (
            "date",
            "",
            "",
            "    sort:\n      - property: property.date\n        direction: ASC\n",
            FILES,
        ),
    ] {
        if workload
            .as_deref()
            .is_some_and(|workload| workload != label)
        {
            continue;
        }
        let input = QueryInput {
            text: format!(
                "{filters}{formulas}views:\n  - type: table\n    name: Table\n    order: [file.name, rank, status]\n{sort}"
            ),
            source_id: "benchmark".to_owned(),
            host_path: "folder-0/note-0.md".to_owned(),
            view_name: None,
            preview_rows: 20,
        };
        let run = || {
            let output = execute(
                &index,
                limits,
                1,
                1,
                &mut false,
                &environment,
                input.clone(),
            )
            .unwrap();
            assert_eq!(output.result.matched_count, count);
            assert_eq!(output.rows.len(), count);
            std::hint::black_box(output);
        };
        for _ in 0..BATCH {
            run();
        }
        let mut samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let started = Instant::now();
            for _ in 0..BATCH {
                run();
            }
            samples.push(started.elapsed().as_secs_f64() * 1000.0 / BATCH as f64);
        }
        println!("{label}: samples_ms={samples:.3?}");
        samples.sort_by(f64::total_cmp);
        println!(
            "{label}: median_ms={:.3}, min_ms={:.3}, max_ms={:.3}",
            samples[SAMPLES / 2],
            samples[0],
            samples[SAMPLES - 1],
        );
    }
}

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
        .initialize(InitialiseParams {
            vault_root: root.path().to_string_lossy().into_owned(),
            metadata_overrides: BTreeMap::new(),
            limits: LimitsPatch {
                query_ms: Some(NonZeroU64::new(10_000).unwrap()),
                result_rows: Some(NonZeroUsize::new(10_000).unwrap()),
                ..LimitsPatch::default()
            },
        })
        .unwrap();
    let result = service
        .query(
            QueryParams {
                source: QuerySource::Inline {
                    text: "views:\n  - type: table\n    name: Table\n    order: []\n".to_owned(),
                    source_id: None,
                },
                host_path: "Host.md".to_owned(),
                view_name: Some("Table".to_owned()),
                preview_rows: 1,
            },
            &environment(),
        )
        .unwrap();
    assert_eq!(result.matched_count, 5_001);
    assert_eq!(result.view_count, 5_001);
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn builds_backlinks_for_five_thousand_fallback_and_broken_links() {
    let root = TempDir::new().unwrap();
    let mut overlays = BTreeMap::new();
    for index in 0..5_000 {
        overlays.insert(
            format!("note-{index:04}.md"),
            Overlay {
                contents: format!("[[Target]] [[Missing-{index}]]"),
                created_at: 0,
            },
        );
    }
    overlays.insert(
        "Targets/Target.md".to_owned(),
        Overlay {
            contents: String::new(),
            created_at: 0,
        },
    );
    let started = Instant::now();
    let index = Index::build(
        root.path().to_owned(),
        &overlays,
        &Default::default(),
        Limits::default(),
        Vec::new(),
    )
    .unwrap();
    let elapsed = started.elapsed();
    eprintln!("5,001 records, 10,000 fallback/broken links: {elapsed:?}");
    assert_eq!(index.records()["Targets/Target.md"].backlinks.len(), 5_000);
    assert!(elapsed < Duration::from_secs(30));
}

/// Reproducible service-level timing probe, excluded from normal correctness tests.
///
/// Run with `cargo test --release --manifest-path worker/Cargo.toml --test scale
/// benchmark_single_file_changes -- --ignored --nocapture --test-threads=1`.
/// Each note is exactly 1 KiB, with properties, tags, a cross-folder basename
/// link and an explicit vault-relative wikilink. The current resolver falls back
/// to basename matching for both links because it first tries source-relative paths.
/// Filesystem setup and writes are excluded from mutation timings. Watcher
/// timings call the service after notification delivery, excluding OS debounce.
#[test]
#[ignore = "manual release-mode benchmark; prints timings without a machine-specific threshold"]
fn benchmark_single_file_changes() {
    const FILES: usize = 5_000;
    const SAMPLES: usize = 3;
    let root = TempDir::new().unwrap();
    for folder in 0..20 {
        fs::create_dir(root.path().join(format!("folder-{folder:02}"))).unwrap();
    }
    let note_path = |index: usize| format!("folder-{:02}/note-{index:04}.md", index % 20);
    let note_contents = |index: usize, revision: usize| {
        let mut contents = format!(
            "---\nrank: {index}\nstatus: active\nrevision: {revision}\ntags: [bench, project]\n---\n# Note {index}\n\n#bench/body [[note-{:04}]] [[{}]]\n\n",
            (index + 1) % FILES,
            note_path((index + 2) % FILES),
        );
        contents.push_str(&"Vault benchmark prose. ".repeat(50));
        contents.truncate(1023);
        contents.push('\n');
        assert_eq!(contents.len(), 1024);
        contents
    };
    for index in 0..FILES {
        fs::write(root.path().join(note_path(index)), note_contents(index, 0)).unwrap();
    }
    let mut service = WorkerService::new();
    let started = Instant::now();
    let initialized = service
        .initialize(InitialiseParams {
            vault_root: root.path().to_string_lossy().into_owned(),
            metadata_overrides: BTreeMap::new(),
            limits: LimitsPatch {
                query_ms: Some(NonZeroU64::new(10_000).unwrap()),
                result_rows: Some(NonZeroUsize::new(10_000).unwrap()),
                ..LimitsPatch::default()
            },
        })
        .unwrap();
    println!(
        "initialize: {:.3} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(initialized.files, FILES);
    let environment = environment();
    let path = note_path(0);
    let upsert = |service: &mut WorkerService, revision: usize| {
        service
            .overlay_upsert(
                OverlayUpsertParams {
                    path: path.clone(),
                    contents: note_contents(0, revision),
                },
                &environment,
            )
            .unwrap();
    };
    let remove = |service: &mut WorkerService| {
        service
            .overlay_remove(OverlayPathParams { path: path.clone() })
            .unwrap();
    };
    let query = |service: &mut WorkerService, ordinal: usize| {
        let result = service
            .query(
                QueryParams {
                    source: QuerySource::Inline {
                        text: "filters: file.hasTag(\"#bench\")\nviews:\n  - type: table\n    name: Table\n    order: [file.name, rank, status]\n    sort:\n      - property: rank\n        direction: ASC\n".to_owned(),
                        source_id: Some(format!("bench-{ordinal}")),
                    },
                    host_path: path.clone(),
                    view_name: Some("Table".to_owned()),
                    preview_rows: 20,
                },
                &environment,
            )
            .unwrap();
        assert_eq!(result.matched_count, FILES);
        assert_eq!(result.view_count, FILES);
    };
    let report = |label: &str, samples: &[f64]| {
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        println!(
            "{label}: samples_ms={samples:.3?}, median_ms={:.3}, min_ms={:.3}, max_ms={:.3}",
            sorted[sorted.len() / 2],
            sorted[0],
            sorted[sorted.len() - 1],
        );
    };

    // Warm the service and filesystem caches before measuring repeated edits.
    upsert(&mut service, 1);
    query(&mut service, 0);
    remove(&mut service);
    service.reindex_external(vec![path.clone()]).unwrap();
    let mut upserts = Vec::new();
    let mut removals = Vec::new();
    let mut external = Vec::new();
    for sample in 0..SAMPLES {
        let started = Instant::now();
        upsert(&mut service, sample + 2);
        upserts.push(started.elapsed().as_secs_f64() * 1000.0);
        let started = Instant::now();
        remove(&mut service);
        removals.push(started.elapsed().as_secs_f64() * 1000.0);
        fs::write(root.path().join(&path), note_contents(0, sample + 2)).unwrap();
        let started = Instant::now();
        service.reindex_external(vec![path.clone()]).unwrap();
        external.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    report("overlay_upsert", &upserts);
    report("overlay_remove", &removals);
    report("known_file_reindex", &external);

    // Independent results mimic refreshing one or eight open Base blocks after
    // an index publication; source IDs differ while the query shape is fixed.
    for fanout in [1, 8] {
        let mut mutations = Vec::new();
        let mut queries = Vec::new();
        let mut totals = Vec::new();
        for sample in 0..SAMPLES {
            let started = Instant::now();
            upsert(&mut service, sample + 10);
            let mutation_ms = started.elapsed().as_secs_f64() * 1000.0;
            let query_started = Instant::now();
            for ordinal in 0..fanout {
                query(&mut service, ordinal);
            }
            mutations.push(mutation_ms);
            queries.push(query_started.elapsed().as_secs_f64() * 1000.0);
            totals.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        report(&format!("fanout_{fanout}_mutation"), &mutations);
        report(&format!("fanout_{fanout}_queries"), &queries);
        report(&format!("fanout_{fanout}_total"), &totals);
    }
}

/// Exercise the cached resolver, changed-edge path, and target-set fallback.
/// Complete snapshot replacement/destruction is timed; setup and assertions are not.
#[test]
#[ignore = "manual release-mode benchmark; prints timings without a machine-specific threshold"]
fn benchmark_link_resolution_and_backlink_updates() {
    const FILES: usize = 5_000;
    const SAMPLES: usize = 9;
    let root = TempDir::new().unwrap();
    // Overlay-only leaf files qualify for named refreshes when their parents
    // exist. Missing parents deliberately trigger a full scan instead.
    for folder in ["notes", "Targets", "Earlier"] {
        fs::create_dir(root.path().join(folder)).unwrap();
    }
    let mut overlays = (0..FILES)
        .map(|ordinal| {
            (
                format!("notes/Source{ordinal}.md"),
                Overlay {
                    contents: "[[Target]] [[Target]]\n".into(),
                    created_at: 0,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for path in ["Targets/Target.md", "Targets/Other.md"] {
        overlays.insert(
            path.into(),
            Overlay {
                contents: String::new(),
                created_at: 0,
            },
        );
    }
    let mut index = Index::build(
        root.path().to_owned(),
        &overlays,
        &Default::default(),
        Limits::default(),
        Vec::new(),
    )
    .unwrap();
    let report = |label: &str, samples: &[f64]| {
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        println!(
            "{label}: samples_ms={samples:.3?}, median_ms={:.3}",
            sorted[sorted.len() / 2]
        );
    };
    let mut lookups = Vec::new();
    for _ in 0..SAMPLES {
        let started = Instant::now();
        for _ in 0..1_000 {
            for link in ["Target", "Missing", "../Targets/Target"] {
                std::hint::black_box(index.resolve_link(link, "notes/Source0.md"));
            }
        }
        lookups.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    report("link_lookup_batch", &lookups);
    let refresh = |index: &mut Index, path: &str, overlays: &BTreeMap<String, Overlay>| {
        let hints = [path.to_owned()];
        let started = Instant::now();
        let next = index
            .refresh(
                &hints,
                overlays,
                &Default::default(),
                Limits::default(),
                Vec::new(),
            )
            .unwrap();
        *index = next;
        started.elapsed().as_secs_f64() * 1000.0
    };
    let mut unchanged = Vec::new();
    let mut changed = Vec::new();
    let mut created = Vec::new();
    let mut removed = Vec::new();
    for ordinal in 0..SAMPLES {
        overlays.get_mut("notes/Source0.md").unwrap().contents =
            format!("[[Target]] [[Target]]\nrevision {ordinal}\n");
        // Restore the common two-edge source before timing its body-only edit.
        refresh(&mut index, "notes/Source0.md", &overlays);
        unchanged.push(refresh(&mut index, "notes/Source0.md", &overlays));
        overlays.get_mut("notes/Source0.md").unwrap().contents =
            "[[Other]] [[Other]] [[Target]]\n".into();
        changed.push(refresh(&mut index, "notes/Source0.md", &overlays));
        assert_eq!(
            index.records()["Targets/Target.md"].backlinks.len(),
            FILES * 2 - 1
        );
        assert_eq!(
            index.records()["Targets/Other.md"].backlinks,
            ["notes/Source0.md"; 2]
        );
        overlays.insert(
            "Earlier/Target.md".into(),
            Overlay {
                contents: String::new(),
                created_at: 0,
            },
        );
        created.push(refresh(&mut index, "Earlier/Target.md", &overlays));
        assert!(index.records()["Targets/Target.md"].backlinks.is_empty());
        assert_eq!(
            index.records()["Earlier/Target.md"].backlinks.len(),
            FILES * 2 - 1
        );
        overlays.remove("Earlier/Target.md");
        removed.push(refresh(&mut index, "Earlier/Target.md", &overlays));
        assert_eq!(
            index.records()["Targets/Target.md"].backlinks.len(),
            FILES * 2 - 1
        );
        assert_eq!(
            index.resolve_link("Target", "notes/Source0.md").as_deref(),
            Some("Targets/Target.md")
        );
    }
    report("backlinks_body_edit", &unchanged);
    report("backlinks_edge_edit", &changed);
    report("backlinks_target_create", &created);
    report("backlinks_target_remove", &removed);
}
