//! Schema open-gate and singleton-root binding acceptance tests.

use super::schema::SCHEMA_USER_VERSION;
use super::*;
use rusqlite::Connection;

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn fresh_databases_start_without_a_repositories_table() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(user_version(&store.connection()), SCHEMA_USER_VERSION);
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
fn opening_a_database_at_an_older_user_version_is_refused() {
    for version in [1, 2] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();
        }

        let error = match Store::open(&path) {
            Ok(_) => panic!("a user_version {version} database must not open"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error,
                StoreOpenError::UnsupportedFormat { version: refused } if refused == version
            ),
            "expected UnsupportedFormat, got: {error:?}"
        );
        assert!(
            error.to_string().contains("index the repository again"),
            "the error must name the delete-and-reindex recovery: {error}"
        );

        // The refusal happens before any write, so the database is untouched.
        let conn = Connection::open(&path).unwrap();
        assert_eq!(user_version(&conn), version);
    }
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
