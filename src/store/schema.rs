use rusqlite::Connection;

use super::StoreOpenError;

/// user_version of the schema below. Any other value is refused at open.
pub(super) const SCHEMA_USER_VERSION: i64 = 3;

// Fresh databases start here directly: one repository per database.
// A database at any other user_version is refused with delete-and-reindex
// guidance; nothing older is supported.
pub(super) const INIT_SCHEMA: &str = r#"
CREATE TABLE repository (
    root_path TEXT NOT NULL,
    content_version TEXT NOT NULL DEFAULT '',
    metadata TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE sources (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    locator TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    modified_at INTEGER,
    metadata TEXT NOT NULL DEFAULT '{}',
    UNIQUE(locator)
);

CREATE TABLE retrieval_units (
    id INTEGER PRIMARY KEY,
    source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    evidence_text TEXT NOT NULL,
    routing_text TEXT NOT NULL,
    token_count INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    timestamp INTEGER
);
CREATE INDEX units_by_source ON retrieval_units(source_id);
CREATE INDEX units_by_hash ON retrieval_units(source_id, content_hash);
CREATE INDEX units_by_timestamp ON retrieval_units(timestamp);

CREATE VIRTUAL TABLE units_fts USING fts5(
    evidence_text,
    routing_text,
    content='retrieval_units',
    content_rowid='id'
);

CREATE TRIGGER retrieval_units_ai AFTER INSERT ON retrieval_units BEGIN
    INSERT INTO units_fts(rowid, evidence_text, routing_text)
    VALUES (new.id, new.evidence_text, new.routing_text);
END;
CREATE TRIGGER retrieval_units_ad AFTER DELETE ON retrieval_units BEGIN
    INSERT INTO units_fts(units_fts, rowid, evidence_text, routing_text)
    VALUES ('delete', old.id, old.evidence_text, old.routing_text);
END;
CREATE TRIGGER retrieval_units_au AFTER UPDATE OF evidence_text, routing_text ON retrieval_units BEGIN
    INSERT INTO units_fts(units_fts, rowid, evidence_text, routing_text)
    VALUES ('delete', old.id, old.evidence_text, old.routing_text);
    INSERT INTO units_fts(rowid, evidence_text, routing_text)
    VALUES (new.id, new.evidence_text, new.routing_text);
END;

CREATE TABLE vectors (
    unit_id INTEGER NOT NULL REFERENCES retrieval_units(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('evidence', 'routing')),
    model_version TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    vector BLOB NOT NULL,
    PRIMARY KEY(unit_id, kind, model_version)
);

CREATE TABLE index_runs (
    id INTEGER PRIMARY KEY,
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
    kind TEXT NOT NULL CHECK(kind IN ('file','symbol','commit','session')),
    value TEXT NOT NULL,
    UNIQUE(kind, value)
);

CREATE TABLE unit_anchors (
    unit_id INTEGER NOT NULL REFERENCES retrieval_units(id) ON DELETE CASCADE,
    anchor_id INTEGER NOT NULL REFERENCES anchors(id) ON DELETE CASCADE,
    relationship TEXT NOT NULL,
    PRIMARY KEY (unit_id, anchor_id, relationship)
);
CREATE INDEX unit_anchors_by_anchor ON unit_anchors(anchor_id);

CREATE TABLE index_leases (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    owner TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
"#;

/// Creates the schema when the database is empty and refuses every other
/// stored layout. Refusal happens before any write, so a refused database
/// is left untouched on disk.
pub(super) fn migrate(conn: &Connection) -> Result<(), StoreOpenError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == SCHEMA_USER_VERSION {
        return Ok(());
    }
    if version != 0 {
        return Err(StoreOpenError::UnsupportedFormat { version });
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    // Re-read under the write lock: another connection may have created the
    // schema while this one waited.
    let locked_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if locked_version != 0 {
        conn.execute_batch("ROLLBACK")?;
        return Ok(());
    }
    let applied = conn
        .execute_batch(INIT_SCHEMA)
        .and_then(|()| conn.pragma_update(None, "user_version", SCHEMA_USER_VERSION));
    match applied {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error.into());
        }
    }
    Ok(())
}

pub(super) fn set_wal_mode(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")
}
