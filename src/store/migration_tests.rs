//! Version 2 -> 3 migration and singleton-root binding acceptance tests.

use super::*;
use rusqlite::Connection;

fn open_raw(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE repositories (
            id INTEGER PRIMARY KEY,
            root_path TEXT NOT NULL UNIQUE,
            content_version TEXT NOT NULL DEFAULT '',
            metadata TEXT NOT NULL DEFAULT '{}'
        );
        CREATE TABLE sources (
            id INTEGER PRIMARY KEY,
            repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            locator TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            modified_at INTEGER,
            metadata TEXT NOT NULL DEFAULT '{}',
            UNIQUE(repo_id, locator)
        );
        CREATE TABLE retrieval_units (
            id INTEGER PRIMARY KEY,
            repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
            source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            evidence_text TEXT NOT NULL,
            routing_text TEXT NOT NULL,
            token_count INTEGER NOT NULL,
            content_hash TEXT NOT NULL,
            metadata TEXT NOT NULL DEFAULT '{}',
            created_at INTEGER NOT NULL DEFAULT 0,
            timestamp INTEGER
        );
        CREATE VIRTUAL TABLE units_fts USING fts5(
            evidence_text, routing_text, content='retrieval_units', content_rowid='id'
        );
        CREATE TABLE vectors (
            unit_id INTEGER NOT NULL REFERENCES retrieval_units(id) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            model_version TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            vector BLOB NOT NULL,
            PRIMARY KEY(unit_id, kind, model_version)
        );
        CREATE TABLE index_runs (
            id INTEGER PRIMARY KEY,
            repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
            started_at INTEGER NOT NULL,
            finished_at INTEGER NOT NULL,
            changed_sources INTEGER NOT NULL,
            unchanged_sources INTEGER NOT NULL,
            deleted_sources INTEGER NOT NULL,
            units_added INTEGER NOT NULL,
            units_reused INTEGER NOT NULL,
            units_removed INTEGER NOT NULL,
            embedded INTEGER NOT NULL,
            status TEXT NOT NULL,
            duration_ms INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE anchors (
            id INTEGER PRIMARY KEY,
            repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            value TEXT NOT NULL,
            UNIQUE(repo_id, kind, value)
        );
        CREATE TABLE unit_anchors (
            unit_id INTEGER NOT NULL REFERENCES retrieval_units(id) ON DELETE CASCADE,
            anchor_id INTEGER NOT NULL REFERENCES anchors(id) ON DELETE CASCADE,
            relationship TEXT NOT NULL,
            PRIMARY KEY (unit_id, anchor_id, relationship)
        );
        CREATE TABLE index_leases (
            repo_id INTEGER PRIMARY KEY,
            owner TEXT NOT NULL,
            expires_at INTEGER NOT NULL
        );
        "#,
    )
    .unwrap();
    conn
}

/// Populates a version-2 database with one repository and one unit carrying a
/// vector, an anchor link, an index run, and an expired lease.
fn seed_v2(conn: &Connection, repos: usize) {
    conn.execute_batch("BEGIN").unwrap();
    for index in 0..repos {
        conn.execute(
            "INSERT INTO repositories(id, root_path) VALUES (?1, ?2)",
            rusqlite::params![index + 1, format!("/repo-{index}")],
        )
        .unwrap();
    }
    conn.execute_batch(
        r#"
        INSERT INTO sources(id, repo_id, kind, locator, content_hash)
            VALUES (1, 1, 'code', 'src/a.rs', 'hash-src');
        INSERT INTO retrieval_units(id, repo_id, source_id, kind, evidence_text, routing_text,
            token_count, content_hash)
            VALUES (10, 1, 1, 'code', 'fn login() {}', 'login', 5, 'hash-unit');
        INSERT INTO units_fts(rowid, evidence_text, routing_text)
            VALUES (10, 'fn login() {}', 'login');
        INSERT INTO vectors VALUES (10, 'evidence', 'mock-v1', 2, x'0000803f0000803f');
        INSERT INTO anchors(id, repo_id, kind, value) VALUES (7, 1, 'symbol', 'login');
        INSERT INTO unit_anchors VALUES (10, 7, 'defines');
        INSERT INTO index_runs(id, repo_id, started_at, finished_at, changed_sources,
            unchanged_sources, deleted_sources, units_added, units_reused, units_removed,
            embedded, status) VALUES (3, 1, 1, 2, 1, 0, 0, 1, 0, 0, 0, 'ok');
        INSERT INTO index_leases VALUES (1, 'stale', 0);
        "#,
    )
    .unwrap();
    conn.execute_batch("COMMIT").unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn fresh_databases_start_at_version_3_without_a_repositories_table() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(user_version(&store.connection()), 3);
    let has_repositories: bool = store
        .connection()
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name='repositories')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!has_repositories);
}

#[test]
fn a_v2_database_with_one_repository_migrates_to_v3() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("v2.db");
    let conn = open_raw(&path);
    seed_v2(&conn, 1);
    drop(conn);

    let store = Store::open(&path).unwrap();
    assert_eq!(user_version(&store.connection()), 3);

    let repository = store.repository().unwrap().expect("singleton survives");
    assert_eq!(repository.root_path, "/repo-0");
    assert_eq!(repository.content_version, "");

    let unit = store.unit_by_id(10).unwrap().expect("unit survives");
    assert_eq!(unit.evidence_text, "fn login() {}");
    assert_eq!(unit.locator, "src/a.rs");

    let vector = store
        .get_vector(10, "evidence", "mock-v1")
        .unwrap()
        .expect("vector survives");
    assert_eq!(vector, vec![1.0f32, 1.0]);

    let anchors = store.anchors_for_unit(10).unwrap();
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].value, "login");

    let run = store.stats().unwrap().last_index_run.expect("run survives");
    assert_eq!(run.status, "ok");

    // The only stored lease had expired, so the v3 lease table stays empty.
    assert!(acquire_lease_for_test(&store));
    assert_eq!(store.source_locators().unwrap(), vec!["src/a.rs"]);

    let violations: Vec<String> = store
        .connection()
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(violations.is_empty(), "foreign_key_check: {violations:?}");
}

fn acquire_lease_for_test(store: &Store) -> bool {
    store.acquire_lease("probe", 60).unwrap()
}

#[test]
fn a_v2_database_with_multiple_repositories_is_rejected_without_migration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("multi.db");
    let conn = open_raw(&path);
    seed_v2(&conn, 2);
    drop(conn);

    let error = match Store::open(&path) {
        Ok(_) => panic!("a multi-repository v2 database must not open"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            StoreOpenError::MultipleRepositories { repositories: 2 }
        ),
        "expected MultipleRepositories, got: {error:?}"
    );
    assert!(
        error.to_string().contains("one repository per database"),
        "the error must tell the user how to recover: {error}"
    );

    // The rejection leaves the version-2 database untouched.
    let conn = Connection::open(&path).unwrap();
    assert_eq!(user_version(&conn), 2);
    let repos: i64 = conn
        .query_row("SELECT count(*) FROM repositories", [], |row| row.get(0))
        .unwrap();
    assert_eq!(repos, 2);
}

#[test]
fn binding_a_second_root_is_rejected_and_keeps_the_first_root() {
    let store = Store::open_in_memory().unwrap();
    let bound = store.bind_repository("/repo-a").unwrap();
    assert_eq!(bound.root_path, "/repo-a");

    let error = store.bind_repository("/repo-b").unwrap_err();
    assert!(
        matches!(error, StoreOpenError::RootMismatch { ref bound, .. } if bound == "/repo-a"),
        "expected RootMismatch, got: {error:?}"
    );
    assert_eq!(store.repository().unwrap().unwrap().root_path, "/repo-a");
    assert_eq!(store.stats().unwrap().sources, 0, "nothing was written");
}

#[test]
fn binding_the_same_root_twice_is_idempotent() {
    let store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    let again = store.bind_repository("/repo").unwrap();
    assert_eq!(again.root_path, "/repo");
}
