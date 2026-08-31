use super::*;
use crate::core::{AnchorKind, BuiltAnchor, BuiltUnit, ResolvedAnchor, SourceKind, UnitKind};
use crate::ingest::{index_repository_bounded, LockedError};
use rusqlite::OptionalExtension;

fn unit(kind: UnitKind, evidence: &str, anchors: Vec<BuiltAnchor>) -> BuiltUnit {
    BuiltUnit {
        kind,
        evidence_text: evidence.to_string(),
        routing_text: evidence.to_string(),
        token_count: 3,
        content_hash: crate::core::hash_segments(&[evidence]),
        metadata: serde_json::json!({}),
        anchors,
    }
}

fn file_anchor(path: &str) -> BuiltAnchor {
    BuiltAnchor {
        kind: AnchorKind::File,
        value: path.to_string(),
        relationship: "touched".to_string(),
    }
}

fn commit_units(store: &mut Store, locator: &str, units: &[BuiltUnit]) -> CommitOutcome {
    store
        .commit_source(SourceIngest {
            kind: SourceKind::Code,
            locator,
            content_hash: "source-hash",
            modified_at: None,
            metadata: serde_json::json!({}),
            units,
        })
        .unwrap()
}

#[test]
fn migrate_is_idempotent_and_vec_loads() {
    let store = Store::open_in_memory().unwrap();
    migrate(store.connection()).unwrap();
    let vec_version: String = store
        .connection()
        .query_row("SELECT vec_version()", [], |row| row.get(0))
        .unwrap();
    assert!(vec_version.starts_with('v'));
    let version: i64 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let busy_timeout: i64 = store
        .connection()
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .unwrap();
    assert_eq!(busy_timeout, 5000);
}

#[test]
fn fresh_databases_create_the_current_schema() {
    let store = Store::open_in_memory().unwrap();
    let has_table = |name: &str| -> bool {
        store
            .connection()
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap()
            .is_some()
    };
    assert!(has_table("anchors"));
    assert!(has_table("unit_anchors"));
    assert!(!has_table("atoms"));
    assert!(!has_table("retrieval_unit_atoms"));
    assert!(!has_table("files"));
    assert!(!has_table("symbols"));
    assert!(!has_table("commits"));
    assert!(!has_table("sessions"));
}

#[test]
fn unit_anchors_are_rebuilt_not_merged_on_recommit() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    commit_units(
        &mut store,
        "src/a.rs",
        &[unit(
            UnitKind::Code,
            "evidence",
            vec![file_anchor("src/a.rs")],
        )],
    );
    let unit_id = store.units_for_source("src/a.rs").unwrap()[0].id.0;

    // Same content hash reuses the unit, but the anchor set must be replaced.
    commit_units(
        &mut store,
        "src/a.rs",
        &[unit(
            UnitKind::Code,
            "evidence",
            vec![file_anchor("src/b.rs")],
        )],
    );
    let anchors = store.anchors_for_unit(unit_id).unwrap();
    assert_eq!(anchors.len(), 1, "stale anchors are cleared: {anchors:?}");
    assert_eq!(anchors[0].value, "src/b.rs");
}

#[test]
fn anchors_resolve_kind_value_and_relationship() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    commit_units(
        &mut store,
        "src/a.rs",
        &[unit(
            UnitKind::Code,
            "evidence",
            vec![
                BuiltAnchor {
                    kind: AnchorKind::Symbol,
                    value: "login".to_string(),
                    relationship: "defines".to_string(),
                },
                file_anchor("src/a.rs"),
            ],
        )],
    );
    let unit_id = store.units_for_source("src/a.rs").unwrap()[0].id.0;
    let anchors = store.anchors_for_unit(unit_id).unwrap();
    assert_eq!(
        anchors,
        vec![
            ResolvedAnchor {
                kind: AnchorKind::File,
                value: "src/a.rs".to_string(),
                relationship: "touched".to_string(),
            },
            ResolvedAnchor {
                kind: AnchorKind::Symbol,
                value: "login".to_string(),
                relationship: "defines".to_string(),
            },
        ]
    );
    assert_eq!(
        store.units_for_anchor("file", "src/a.rs", 10).unwrap(),
        (vec![unit_id], 0)
    );
    assert_eq!(
        store.units_for_anchor("bogus", "x", 10).unwrap(),
        (Vec::new(), 0)
    );
}

#[test]
fn vector_models_lists_per_model_counts() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    let unit_ids = commit_units(
        &mut store,
        "src/a.rs",
        &[unit(UnitKind::Code, "evidence", Vec::new())],
    );
    let unit_id = store.units_for_source("src/a.rs").unwrap()[0].id.0;
    assert_eq!(unit_ids.units_added, 1);
    store
        .put_vector(unit_id, "evidence", "mock-v1", &[0.1, 0.2])
        .unwrap();
    store
        .put_vector(unit_id, "routing", "mock-v1", &[0.3, 0.4])
        .unwrap();
    store
        .put_vector(
            unit_id,
            "evidence",
            "Qwen3-Embedding-0.6B-Q8_0",
            &[0.5, 0.6],
        )
        .unwrap();
    assert_eq!(
        store.vector_models().unwrap(),
        vec![
            ("Qwen3-Embedding-0.6B-Q8_0".to_string(), 1),
            ("mock-v1".to_string(), 2),
        ]
    );
}

fn routing_unit(routing_text: &str) -> BuiltUnit {
    BuiltUnit {
        kind: UnitKind::Prose,
        evidence_text: "same evidence".to_string(),
        routing_text: routing_text.to_string(),
        token_count: 4,
        content_hash: "stable-hash".to_string(),
        metadata: serde_json::json!({}),
        anchors: Vec::new(),
    }
}

fn commit_routing_unit(store: &mut Store, unit: &BuiltUnit) {
    store
        .commit_source(SourceIngest {
            kind: SourceKind::Text,
            locator: "snoop://routed",
            content_hash: "source-hash",
            modified_at: None,
            metadata: serde_json::json!({}),
            units: std::slice::from_ref(unit),
        })
        .unwrap();
}

#[test]
fn reused_unit_with_changed_routing_text_loses_stale_routing_vectors() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    commit_routing_unit(&mut store, &routing_unit("old routing text"));
    let unit_id = store.units_for_source("snoop://routed").unwrap()[0].id.0;
    store
        .put_vector(unit_id, "routing", "m1", &[0.1, 0.2])
        .unwrap();
    store
        .put_vector(unit_id, "evidence", "m1", &[0.3, 0.4])
        .unwrap();

    commit_routing_unit(&mut store, &routing_unit("fresh routing text"));
    let reloaded = store.units_for_source("snoop://routed").unwrap();
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded[0].id.0, unit_id, "hash reuse keeps the unit id");
    assert_eq!(reloaded[0].routing_text, "fresh routing text");
    assert!(
        store
            .get_vector(unit_id, "routing", "m1")
            .unwrap()
            .is_none(),
        "stale routing vector is deleted so it regenerates"
    );
    assert!(
        store
            .get_vector(unit_id, "evidence", "m1")
            .unwrap()
            .is_some(),
        "unchanged evidence text keeps its vector"
    );
    let missing = store
        .units_missing_vectors_page("routing", "m1", 0, 32)
        .unwrap();
    assert_eq!(
        missing,
        vec![(unit_id, "fresh routing text".to_string())],
        "the reused unit is re-embedded from the new routing text"
    );

    store
        .put_vector(unit_id, "routing", "m1", &[0.5, 0.6])
        .unwrap();
    commit_routing_unit(&mut store, &routing_unit("fresh routing text"));
    assert!(
        store
            .get_vector(unit_id, "routing", "m1")
            .unwrap()
            .is_some(),
        "an unchanged routing text never deletes vectors"
    );
    assert!(store
        .units_missing_vectors_page("routing", "m1", 0, 32)
        .unwrap()
        .is_empty());
}

#[test]
fn persistent_database_reopens_with_sqlite_vec() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("index.db");
    let first = Store::open(&path).unwrap();
    drop(first);
    let reopened = Store::open(&path).unwrap();
    let version: String = reopened
        .connection()
        .query_row("SELECT vec_version()", [], |row| row.get(0))
        .unwrap();
    assert!(version.starts_with('v'));
}

#[test]
fn concurrent_open_of_a_shared_fresh_database_succeeds() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("race.db");
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || Store::open(&path).is_ok())
        })
        .collect();
    for handle in handles {
        assert!(
            handle.join().unwrap(),
            "concurrent Store::open must create the schema without duplicate-DDL errors"
        );
    }
    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
}

#[test]
fn dropped_transaction_writes_nothing() {
    let mut store = Store::open_in_memory().unwrap();
    let transaction = store.conn.transaction().unwrap();
    transaction
        .execute(
            "INSERT INTO sources(kind, locator, content_hash) VALUES ('code', 'x.rs', 'h')",
            [],
        )
        .unwrap();
    drop(transaction);
    assert_eq!(store.stats().unwrap().sources, 0);
}

#[test]
fn record_index_run_writes_status_and_real_started_at() {
    let store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();

    store
        .record_index_run(&IndexRunStats {
            changed_sources: 2,
            duration_ms: 5000,
            ..Default::default()
        })
        .unwrap();
    let (started_at, finished_at, status): (i64, i64, String) = store
        .connection()
        .query_row(
            "SELECT started_at,finished_at,status FROM index_runs ORDER BY id ASC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "ok");
    assert_eq!(
        finished_at - started_at,
        5,
        "started_at reflects the reported run duration"
    );

    store
        .record_index_run(&IndexRunStats {
            status: IndexRunStatus::Timeout,
            duration_ms: 300_000,
            ..Default::default()
        })
        .unwrap();
    let run = store
        .stats()
        .unwrap()
        .last_index_run
        .expect("latest run is surfaced");
    assert_eq!(run.status, "timeout", "snoop status shows run status");
    assert_eq!(run.duration_ms, 300_000);
}

#[test]
fn locked_refusal_is_typed_writes_no_run_row_and_keeps_holder() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("README.md"),
        "# Auth\n\nRefresh the session token.\n",
    )
    .unwrap();

    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository(&root.to_string_lossy()).unwrap();
    assert!(store.acquire_lease("blocker", 3600).unwrap());
    let runs_before = store.stats().unwrap().index_runs;

    let error = index_repository_bounded(&mut store, &root, None, None).unwrap_err();
    assert!(
        error.is::<LockedError>(),
        "the refusal must be the typed LockedError, got: {error}"
    );
    assert_eq!(
        store.stats().unwrap().index_runs,
        runs_before,
        "a Locked refusal writes no index_runs row"
    );
    let (owner, _) = super::leases::lease_row(&store).unwrap();
    assert_eq!(owner, "blocker", "the holder's lease is untouched");
}

#[test]
fn dense_reuse_relocates_rows_through_the_id_map() {
    // Audit finding 4 (run 20260830195149-6f1a96a5): reused rows relocate
    // via an id map; row lookups per commit == U_reused (was a linear scan
    // per reused unit, exact U(U+1)/2 dense scans).
    // Budget (advisory, machine-dependent): dense rewrite wall <= miss wall
    // at U = 16384 (~0.9 s).
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    let units: Vec<BuiltUnit> = (0..600)
        .map(|i| unit(UnitKind::Code, &format!("unit-{i}"), Vec::new()))
        .collect();
    let first = commit_units(&mut store, "src/dense.rs", &units);
    assert_eq!(first.units_added, 600);
    let again = commit_units(&mut store, "src/dense.rs", &units);
    assert_eq!(again.units_added, 0);
    assert_eq!(
        again.units_reused, 600,
        "dense rewrite relocates every row through the id map"
    );
    assert_eq!(again.units_removed, 0);
}
