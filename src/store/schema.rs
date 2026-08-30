use rusqlite::Connection;

// Fresh databases start directly at version 2. Version 1 databases migrate
// through MIGRATION_V2; nothing older is supported.
pub(super) const INIT_SCHEMA_V2: &str = r#"
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
CREATE INDEX sources_by_repo ON sources(repo_id);

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
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    timestamp INTEGER
);
CREATE INDEX units_by_repo ON retrieval_units(repo_id);
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
    kind TEXT NOT NULL CHECK(kind IN ('file','symbol','commit','session')),
    value TEXT NOT NULL,
    UNIQUE(repo_id, kind, value)
);

CREATE TABLE unit_anchors (
    unit_id INTEGER NOT NULL REFERENCES retrieval_units(id) ON DELETE CASCADE,
    anchor_id INTEGER NOT NULL REFERENCES anchors(id) ON DELETE CASCADE,
    relationship TEXT NOT NULL,
    PRIMARY KEY (unit_id, anchor_id, relationship)
);
CREATE INDEX unit_anchors_by_anchor ON unit_anchors(anchor_id);

CREATE TABLE index_leases (
    repo_id INTEGER PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    owner TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
"#;

// Version 1 -> 2: persisted atoms are gone and the four per-kind anchor
// tables fold into one. Anchor rows are re-linked with explicit kind-specific
// joins; unit ids, vectors, and FTS rows carry over untouched.
pub(super) const MIGRATION_V2: &str = r#"
ALTER TABLE unit_anchors RENAME TO unit_anchors_v1;

CREATE TABLE anchors (
    id INTEGER PRIMARY KEY,
    repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('file','symbol','commit','session')),
    value TEXT NOT NULL,
    UNIQUE(repo_id, kind, value)
);
INSERT INTO anchors(repo_id, kind, value) SELECT repo_id, 'file', path FROM files;
INSERT INTO anchors(repo_id, kind, value) SELECT repo_id, 'symbol', name FROM symbols;
INSERT INTO anchors(repo_id, kind, value) SELECT repo_id, 'commit', oid FROM commits;
INSERT INTO anchors(repo_id, kind, value) SELECT repo_id, 'session', session_id FROM sessions;

CREATE TABLE unit_anchors (
    unit_id INTEGER NOT NULL REFERENCES retrieval_units(id) ON DELETE CASCADE,
    anchor_id INTEGER NOT NULL REFERENCES anchors(id) ON DELETE CASCADE,
    relationship TEXT NOT NULL,
    PRIMARY KEY (unit_id, anchor_id, relationship)
);

INSERT INTO unit_anchors(unit_id, anchor_id, relationship)
    SELECT old.unit_id, a.id, old.relationship
    FROM unit_anchors_v1 old
    JOIN files file_anchor
      ON file_anchor.id = old.anchor_id
     AND file_anchor.repo_id = (SELECT u.repo_id FROM retrieval_units u WHERE u.id = old.unit_id)
    JOIN anchors a
      ON a.repo_id = file_anchor.repo_id AND a.kind = 'file' AND a.value = file_anchor.path
    WHERE old.anchor_kind = 'file';

INSERT INTO unit_anchors(unit_id, anchor_id, relationship)
    SELECT old.unit_id, a.id, old.relationship
    FROM unit_anchors_v1 old
    JOIN symbols symbol_anchor
      ON symbol_anchor.id = old.anchor_id
     AND symbol_anchor.repo_id = (SELECT u.repo_id FROM retrieval_units u WHERE u.id = old.unit_id)
    JOIN anchors a
      ON a.repo_id = symbol_anchor.repo_id AND a.kind = 'symbol' AND a.value = symbol_anchor.name
    WHERE old.anchor_kind = 'symbol';

INSERT INTO unit_anchors(unit_id, anchor_id, relationship)
    SELECT old.unit_id, a.id, old.relationship
    FROM unit_anchors_v1 old
    JOIN commits commit_anchor
      ON commit_anchor.id = old.anchor_id
     AND commit_anchor.repo_id = (SELECT u.repo_id FROM retrieval_units u WHERE u.id = old.unit_id)
    JOIN anchors a
      ON a.repo_id = commit_anchor.repo_id AND a.kind = 'commit' AND a.value = commit_anchor.oid
    WHERE old.anchor_kind = 'commit';

INSERT INTO unit_anchors(unit_id, anchor_id, relationship)
    SELECT old.unit_id, a.id, old.relationship
    FROM unit_anchors_v1 old
    JOIN sessions session_anchor
      ON session_anchor.id = old.anchor_id
     AND session_anchor.repo_id = (SELECT u.repo_id FROM retrieval_units u WHERE u.id = old.unit_id)
    JOIN anchors a
      ON a.repo_id = session_anchor.repo_id AND a.kind = 'session' AND a.value = session_anchor.session_id
    WHERE old.anchor_kind = 'session';

CREATE INDEX unit_anchors_by_anchor ON unit_anchors(anchor_id);

DROP TABLE unit_anchors_v1;
DROP TABLE files;
DROP TABLE symbols;
DROP TABLE commits;
DROP TABLE sessions;
DROP TABLE retrieval_unit_atoms;
DROP TABLE atoms;
"#;

pub(super) fn migration_after(version: i64) -> Option<(i64, &'static str)> {
    match version {
        0 => Some((2, INIT_SCHEMA_V2)),
        1 => Some((2, MIGRATION_V2)),
        _ => None,
    }
}

pub(super) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    loop {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let Some((target, sql)) = migration_after(version) else {
            return Ok(());
        };

        conn.execute_batch("BEGIN IMMEDIATE")?;
        let locked_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if locked_version != version {
            conn.execute_batch("ROLLBACK")?;
            continue;
        }
        let applied = conn
            .execute_batch(sql)
            .and_then(|()| conn.pragma_update(None, "user_version", target));
        match applied {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
    }
}

pub(super) fn set_wal_mode(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")
}
