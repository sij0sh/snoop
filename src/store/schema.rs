use rusqlite::Connection;

use super::StoreOpenError;

// Fresh databases start directly at version 3: one repository per database.
// Version 1 databases migrate through MIGRATION_V2 then MIGRATION_V3;
// nothing older is supported.
pub(super) const INIT_SCHEMA_V3: &str = r#"
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

// Version 2 -> 3, copy-out phase: one repository per database. Unit, vector,
// anchor, and run ids carry over unchanged, so unit_anchors links stay valid.
// Runs only when the database holds at most one repository; otherwise it fails
// without touching the schema. Requires foreign_keys=OFF around the rebuild
// because dropping and recreating parents would otherwise cascade.
pub(super) const COPY_OUT_V3: &str = r#"
CREATE TABLE repository_tmp AS
    SELECT root_path, content_version, metadata FROM repositories;

CREATE TABLE sources_tmp AS SELECT * FROM sources;
CREATE TABLE units_tmp AS SELECT id, source_id, kind, evidence_text, routing_text,
    token_count, content_hash, metadata, created_at, timestamp FROM retrieval_units;
CREATE TABLE runs_tmp AS SELECT * FROM index_runs;
CREATE TABLE anchors_tmp AS SELECT id, kind, value FROM anchors;
CREATE TABLE unit_anchors_tmp AS SELECT unit_id, anchor_id, relationship FROM unit_anchors;
CREATE TABLE vectors_tmp AS SELECT unit_id, kind, model_version, dimensions, vector FROM vectors;
CREATE TABLE leases_tmp AS SELECT owner, expires_at FROM index_leases
    WHERE expires_at > unixepoch();

DROP TABLE IF EXISTS units_fts;
DROP TABLE IF EXISTS vectors;
DROP TABLE IF EXISTS unit_anchors;
DROP TABLE IF EXISTS anchors;
DROP TABLE IF EXISTS index_leases;
DROP TABLE IF EXISTS index_runs;
DROP TABLE IF EXISTS retrieval_units;
DROP TABLE IF EXISTS sources;
DROP TABLE IF EXISTS repositories;
"#;

// Version 2 -> 3, copy-back phase.
pub(super) const COPY_BACK_V3: &str = r#"
INSERT INTO repository(root_path, content_version, metadata)
    SELECT root_path, content_version, metadata FROM repository_tmp;
INSERT INTO sources(id,kind,locator,content_hash,modified_at,metadata)
    SELECT id,kind,locator,content_hash,modified_at,metadata FROM sources_tmp;
INSERT INTO retrieval_units(id,source_id,kind,evidence_text,routing_text,
    token_count,content_hash,metadata,created_at,timestamp)
    SELECT id,source_id,kind,evidence_text,routing_text,
    token_count,content_hash,metadata,created_at,timestamp FROM units_tmp;
INSERT INTO index_runs(id,started_at,finished_at,changed_sources,unchanged_sources,
    deleted_sources,units_added,units_reused,units_removed,embedded,status,duration_ms)
    SELECT id,started_at,finished_at,changed_sources,unchanged_sources,
    deleted_sources,units_added,units_reused,units_removed,embedded,status,duration_ms FROM runs_tmp;
INSERT INTO anchors(id,kind,value) SELECT id,kind,value FROM anchors_tmp;
INSERT INTO unit_anchors(unit_id,anchor_id,relationship)
    SELECT unit_id,anchor_id,relationship FROM unit_anchors_tmp;
INSERT INTO vectors(unit_id,kind,model_version,dimensions,vector)
    SELECT unit_id,kind,model_version,dimensions,vector FROM vectors_tmp;
INSERT INTO index_leases(id,owner,expires_at)
    SELECT 1, owner, expires_at FROM leases_tmp ORDER BY expires_at DESC LIMIT 1;

DROP TABLE sources_tmp;
DROP TABLE units_tmp;
DROP TABLE runs_tmp;
DROP TABLE anchors_tmp;
DROP TABLE unit_anchors_tmp;
DROP TABLE vectors_tmp;
DROP TABLE leases_tmp;
DROP TABLE repository_tmp;
"#;

fn apply(conn: &Connection, version: i64, target: i64, sql: &str) -> Result<(), StoreOpenError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let locked_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if locked_version != version {
        conn.execute_batch("ROLLBACK")?;
        return Ok(());
    }
    let applied = conn
        .execute_batch(sql)
        .and_then(|()| conn.pragma_update(None, "user_version", target));
    match applied {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error.into());
        }
    }
    Ok(())
}

fn migrate_v2_to_v3(conn: &Connection) -> Result<(), StoreOpenError> {
    conn.execute_batch("PRAGMA foreign_keys=OFF")?;
    let result = (|| {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let locked_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if locked_version != 2 {
            conn.execute_batch("ROLLBACK")?;
            return Ok(());
        }
        let repositories: i64 =
            conn.query_row("SELECT count(*) FROM repositories", [], |row| row.get(0))?;
        if repositories > 1 {
            conn.execute_batch("ROLLBACK")?;
            return Err(StoreOpenError::MultipleRepositories { repositories });
        }
        let batch = format!("{COPY_OUT_V3}\n{INIT_SCHEMA_V3}\n{COPY_BACK_V3}");
        conn.execute_batch(&batch)
            .and_then(|()| conn.pragma_update(None, "user_version", 3))?;
        conn.execute_batch("COMMIT")?;
        Ok(())
    })();
    let restore = conn.execute_batch("PRAGMA foreign_keys=ON");
    result.and(restore.map_err(StoreOpenError::from))
}

pub(super) fn migrate(conn: &Connection) -> Result<(), StoreOpenError> {
    loop {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        match version {
            0 => apply(conn, 0, 3, INIT_SCHEMA_V3)?,
            1 => apply(conn, 1, 2, MIGRATION_V2)?,
            2 => migrate_v2_to_v3(conn)?,
            _ => return Ok(()),
        }
    }
}

pub(super) fn set_wal_mode(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")
}
