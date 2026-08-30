use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::core::{
    AnchorKind, AtomId, BuiltUnit, ParsedAtom, RepoId, Repository, RetrievalUnit, Source, SourceId,
    SourceKind, UnitId, UnitKind,
};

pub struct Store {
    conn: Connection,
}

type SqliteExtensionEntry = unsafe extern "C" fn(
    *mut rusqlite::ffi::sqlite3,
    *mut *mut std::ffi::c_char,
    *const rusqlite::ffi::sqlite3_api_routines,
) -> std::ffi::c_int;

fn register_sqlite_vec() -> rusqlite::Result<()> {
    static RESULT: OnceLock<i32> = OnceLock::new();
    let code = *RESULT.get_or_init(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            SqliteExtensionEntry,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )))
    });
    if code == rusqlite::ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            Some("failed to register sqlite-vec".to_string()),
        ))
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

// Single init schema. Squashes the former MIGRATION_V1..V8 ladder; databases
// at versions 1..=7 predate this schema and are discarded and rebuilt.
const INIT_SCHEMA: &str = r#"
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

CREATE TABLE atoms (
    id INTEGER PRIMARY KEY,
    source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    content_hash TEXT NOT NULL
);
CREATE INDEX atoms_by_source ON atoms(source_id);

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

CREATE TABLE retrieval_unit_atoms (
    unit_id INTEGER NOT NULL REFERENCES retrieval_units(id) ON DELETE CASCADE,
    atom_id INTEGER NOT NULL REFERENCES atoms(id) ON DELETE CASCADE,
    PRIMARY KEY (unit_id, atom_id)
);

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

CREATE TABLE files (
    id INTEGER PRIMARY KEY,
    repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    UNIQUE(repo_id, path)
);

CREATE TABLE symbols (
    id INTEGER PRIMARY KEY,
    repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    UNIQUE(repo_id, name)
);

CREATE TABLE commits (
    id INTEGER PRIMARY KEY,
    repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    oid TEXT NOT NULL,
    UNIQUE(repo_id, oid)
);

CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    UNIQUE(repo_id, session_id)
);

CREATE TABLE unit_anchors (
    unit_id INTEGER NOT NULL REFERENCES retrieval_units(id) ON DELETE CASCADE,
    anchor_kind TEXT NOT NULL CHECK(anchor_kind IN ('file','symbol','commit','session')),
    anchor_id INTEGER NOT NULL,
    relationship TEXT NOT NULL,
    confidence_source TEXT NOT NULL CHECK(confidence_source IN ('deterministic','heuristic')),
    PRIMARY KEY (unit_id, anchor_kind, anchor_id, relationship)
);
CREATE INDEX unit_anchors_by_anchor ON unit_anchors(anchor_kind, anchor_id);

CREATE TABLE index_leases (
    repo_id INTEGER PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    owner TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
"#;

fn migration_after(version: i64) -> Option<(i64, &'static str)> {
    match version {
        0 => Some((1, INIT_SCHEMA)),
        _ => None,
    }
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
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

fn set_wal_mode(conn: &Connection) -> rusqlite::Result<()> {
    for _ in 0..50 {
        let result = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0));
        match result {
            Ok(_) => return Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error),
        }
    }
    Err(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some("timed out setting WAL journal mode".into()),
    ))
}

pub struct SourceIngest<'a> {
    pub repo_id: RepoId,
    pub kind: SourceKind,
    pub locator: &'a str,
    pub content_hash: &'a str,
    pub modified_at: Option<i64>,
    pub metadata: serde_json::Value,
    pub atoms: &'a [ParsedAtom],
    pub units: &'a [BuiltUnit],
}

#[derive(Debug, Clone, Default)]
pub struct CommitOutcome {
    pub source_id: SourceId,
    pub units_added: usize,
    pub units_reused: usize,
    pub units_removed: usize,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct StoreStats {
    pub repositories: i64,
    pub sources: i64,
    pub units: i64,
    pub vectors: i64,
    pub index_runs: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_index_run: Option<LastIndexRun>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LastIndexRun {
    pub finished_at: i64,
    pub duration_ms: i64,
    pub changed_sources: i64,
    pub unchanged_sources: i64,
    pub embedded: i64,
    pub status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexRunStatus {
    #[default]
    Ok,
    Error,
    Timeout,
}

impl IndexRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            IndexRunStatus::Ok => "ok",
            IndexRunStatus::Error => "error",
            IndexRunStatus::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct IndexRunStats {
    pub changed_sources: usize,
    pub unchanged_sources: usize,
    pub deleted_sources: usize,
    pub units_added: usize,
    pub units_reused: usize,
    pub units_removed: usize,
    pub embedded: usize,
    pub duration_ms: u64,
    pub status: IndexRunStatus,
}

fn ensure_anchor(
    transaction: &rusqlite::Transaction<'_>,
    repo_id: RepoId,
    kind: AnchorKind,
    value: &str,
) -> rusqlite::Result<i64> {
    let (table, column) = match kind {
        AnchorKind::File => ("files", "path"),
        AnchorKind::Symbol => ("symbols", "name"),
        AnchorKind::Commit => ("commits", "oid"),
        AnchorKind::Session => ("sessions", "session_id"),
    };
    let sql = format!(
        "INSERT INTO {table}(repo_id,{column}) VALUES (?1,?2) ON CONFLICT(repo_id,{column}) DO NOTHING"
    );
    transaction.execute(&sql, params![repo_id.0, value])?;
    let select = format!("SELECT id FROM {table} WHERE repo_id=?1 AND {column}=?2");
    transaction.query_row(&select, params![repo_id.0, value], |row| row.get(0))
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        register_sqlite_vec()?;
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> rusqlite::Result<Self> {
        register_sqlite_vec()?;
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.busy_timeout(Duration::from_millis(5000))?;
        set_wal_mode(&conn)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn ensure_repository(&self, root_path: &str) -> rusqlite::Result<Repository> {
        self.conn.execute(
            "INSERT INTO repositories(root_path) VALUES (?1) ON CONFLICT(root_path) DO NOTHING",
            params![root_path],
        )?;
        self.repository_by_root(root_path)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    pub fn set_repository_content_version(
        &self,
        repo_id: RepoId,
        version: &str,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE repositories SET content_version=?2 WHERE id=?1",
            params![repo_id.0, version],
        )?;
        Ok(())
    }

    pub fn set_repository_git_tip(&self, repo_id: RepoId, tip: &str) -> rusqlite::Result<()> {
        let mut metadata = self
            .conn
            .query_row(
                "SELECT metadata FROM repositories WHERE id=?1",
                params![repo_id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        metadata["git_tip"] = serde_json::json!(tip);
        self.conn.execute(
            "UPDATE repositories SET metadata=?2 WHERE id=?1",
            params![repo_id.0, metadata.to_string()],
        )?;
        Ok(())
    }

    pub fn git_commit_locators(&self, repo_id: RepoId) -> rusqlite::Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT locator FROM sources WHERE repo_id=?1 AND kind='git_commit'")?;
        let rows = statement.query_map(params![repo_id.0], |row| row.get(0))?;
        rows.collect()
    }

    pub fn repository_by_root(&self, root_path: &str) -> rusqlite::Result<Option<Repository>> {
        self.conn
            .query_row(
                "SELECT id, root_path, content_version, metadata FROM repositories WHERE root_path=?1",
                params![root_path],
                repository_from_row,
            )
            .optional()
    }

    pub fn first_repository(&self) -> rusqlite::Result<Option<Repository>> {
        self.conn
            .query_row(
                "SELECT id, root_path, content_version, metadata FROM repositories ORDER BY id LIMIT 1",
                [],
                repository_from_row,
            )
            .optional()
    }

    pub fn source_by_locator(
        &self,
        repo_id: RepoId,
        locator: &str,
    ) -> rusqlite::Result<Option<Source>> {
        self.conn
            .query_row(
                "SELECT id, repo_id, kind, locator, content_hash, modified_at, metadata
                 FROM sources WHERE repo_id=?1 AND locator=?2",
                params![repo_id.0, locator],
                source_from_row,
            )
            .optional()
    }

    pub fn source_locators(&self, repo_id: RepoId) -> rusqlite::Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT locator FROM sources WHERE repo_id=?1 ORDER BY locator")?;
        let rows = statement.query_map(params![repo_id.0], |row| row.get(0))?;
        rows.collect()
    }

    pub fn delete_sources_not_in(
        &self,
        repo_id: RepoId,
        present: &HashSet<String>,
    ) -> rusqlite::Result<usize> {
        let mut deleted = 0;
        for locator in self.source_locators(repo_id)? {
            if !present.contains(&locator) {
                deleted += self.conn.execute(
                    "DELETE FROM sources WHERE repo_id=?1 AND locator=?2",
                    params![repo_id.0, locator],
                )?;
            }
        }
        Ok(deleted)
    }

    /// Acquires the per-repository index lease; lazily steals an expired one.
    /// Returns false when another indexer holds an unexpired lease.
    pub fn acquire_lease(
        &self,
        repo_id: RepoId,
        owner: &str,
        ttl_secs: i64,
    ) -> rusqlite::Result<bool> {
        let timestamp = now();
        let changed = self.conn.execute(
            "INSERT INTO index_leases(repo_id,owner,expires_at) VALUES (?1,?2,?3)
             ON CONFLICT(repo_id) DO UPDATE SET owner=excluded.owner, expires_at=excluded.expires_at
             WHERE index_leases.expires_at<=?4",
            params![repo_id.0, owner, timestamp + ttl_secs, timestamp],
        )?;
        Ok(changed > 0)
    }

    /// Releases the lease only when the caller owns it.
    pub fn release_lease(&self, repo_id: RepoId, owner: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM index_leases WHERE repo_id=?1 AND owner=?2",
            params![repo_id.0, owner],
        )?;
        Ok(())
    }

    /// Renews the lease only when the caller owns it and it has not expired.
    /// Returns false when the lease is missing, stolen, or expired.
    pub fn renew_lease(
        &self,
        repo_id: RepoId,
        owner: &str,
        ttl_secs: i64,
    ) -> rusqlite::Result<bool> {
        let timestamp = now();
        let changed = self.conn.execute(
            "UPDATE index_leases SET expires_at=?3
             WHERE repo_id=?1 AND owner=?2 AND expires_at>?4",
            params![repo_id.0, owner, timestamp + ttl_secs, timestamp],
        )?;
        Ok(changed > 0)
    }

    pub fn commit_source(&mut self, request: SourceIngest<'_>) -> rusqlite::Result<CommitOutcome> {
        let transaction = self.conn.transaction()?;
        let source_id: i64 = transaction.query_row(
            "INSERT INTO sources(repo_id, kind, locator, content_hash, modified_at, metadata)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(repo_id,locator) DO UPDATE SET
               kind=excluded.kind, content_hash=excluded.content_hash,
               modified_at=excluded.modified_at, metadata=excluded.metadata
             RETURNING id",
            params![
                request.repo_id.0,
                request.kind.as_str(),
                request.locator,
                request.content_hash,
                request.modified_at,
                request.metadata.to_string()
            ],
            |row| row.get(0),
        )?;

        let old_units: Vec<(i64, String, String, String)> = {
            let mut statement = transaction.prepare(
                "SELECT id, content_hash, routing_text, evidence_text
                 FROM retrieval_units WHERE source_id=?1 ORDER BY id",
            )?;
            let rows = statement.query_map(params![source_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        transaction.execute(
            "DELETE FROM retrieval_unit_atoms WHERE unit_id IN
             (SELECT id FROM retrieval_units WHERE source_id=?1)",
            params![source_id],
        )?;

        let mut available: HashMap<String, VecDeque<i64>> = HashMap::new();
        for (id, hash, _, _) in &old_units {
            available.entry(hash.clone()).or_default().push_back(*id);
        }
        let mut kept = HashSet::new();
        let mut unit_rows = Vec::with_capacity(request.units.len());
        let mut added = 0;
        let mut reused = 0;
        for unit in request.units {
            if let Some(id) = available
                .get_mut(&unit.content_hash)
                .and_then(VecDeque::pop_front)
            {
                let old_row = old_units
                    .iter()
                    .find(|(old_id, _, _, _)| *old_id == id)
                    .expect("reused unit id comes from the previous rows");
                transaction.execute(
                    "UPDATE retrieval_units SET kind=?2, evidence_text=?3, routing_text=?4,
                     token_count=?5, metadata=?6, timestamp=?7 WHERE id=?1",
                    params![
                        id,
                        unit.kind.as_str(),
                        unit.evidence_text,
                        unit.routing_text,
                        unit.token_count,
                        unit.metadata.to_string(),
                        unit.metadata["timestamp"].as_i64()
                    ],
                )?;

                if old_row.2 != unit.routing_text {
                    transaction.execute(
                        "DELETE FROM vectors WHERE unit_id=?1 AND kind='routing'",
                        params![id],
                    )?;
                }
                if old_row.3 != unit.evidence_text {
                    transaction.execute(
                        "DELETE FROM vectors WHERE unit_id=?1 AND kind='evidence'",
                        params![id],
                    )?;
                }
                kept.insert(id);
                unit_rows.push((id, unit));
                reused += 1;
            } else {
                let id: i64 = transaction.query_row(
                    "INSERT INTO retrieval_units(repo_id,source_id,kind,evidence_text,routing_text,
                     token_count,content_hash,metadata,timestamp) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                     RETURNING id",
                    params![
                        request.repo_id.0,
                        source_id,
                        unit.kind.as_str(),
                        unit.evidence_text,
                        unit.routing_text,
                        unit.token_count,
                        unit.content_hash,
                        unit.metadata.to_string(),
                        unit.metadata["timestamp"].as_i64()
                    ],
                    |row| row.get(0),
                )?;
                unit_rows.push((id, unit));
                added += 1;
            }
        }
        let mut removed = 0;
        for (id, _, _, _) in old_units {
            if !kept.contains(&id) {
                removed +=
                    transaction.execute("DELETE FROM retrieval_units WHERE id=?1", params![id])?;
            }
        }

        transaction.execute("DELETE FROM atoms WHERE source_id=?1", params![source_id])?;
        let mut atom_ids = Vec::with_capacity(request.atoms.len());
        for atom in request.atoms {
            let id: i64 = transaction.query_row(
                "INSERT INTO atoms(source_id,content_hash) VALUES (?1,?2) RETURNING id",
                params![source_id, atom.content_hash],
                |row| row.get(0),
            )?;
            atom_ids.push(id);
        }
        for (unit_id, unit) in unit_rows {
            for atom_index in &unit.atom_indices {
                transaction.execute(
                    "INSERT INTO retrieval_unit_atoms(unit_id,atom_id) VALUES (?1,?2)",
                    params![unit_id, atom_ids[*atom_index]],
                )?;
            }
            for anchor in &unit.anchors {
                let anchor_id =
                    ensure_anchor(&transaction, request.repo_id, anchor.kind, &anchor.value)?;
                transaction.execute(
                    "INSERT INTO unit_anchors(unit_id,anchor_kind,anchor_id,relationship,confidence_source)
                     VALUES (?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING",
                    params![
                        unit_id,
                        anchor.kind.as_str(),
                        anchor_id,
                        anchor.relationship,
                        anchor.confidence
                    ],
                )?;
            }
        }
        transaction.commit()?;
        Ok(CommitOutcome {
            source_id: SourceId(source_id),
            units_added: added,
            units_reused: reused,
            units_removed: removed,
        })
    }

    pub fn record_index_run(&self, repo_id: RepoId, stats: &IndexRunStats) -> rusqlite::Result<()> {
        let timestamp = now();
        // started_at is real: the runner reports elapsed duration, so start = finish - duration.
        let started_at = timestamp.saturating_sub((stats.duration_ms as i64) / 1000);
        self.conn.execute(
            "INSERT INTO index_runs(repo_id,started_at,finished_at,changed_sources,
             unchanged_sources,deleted_sources,units_added,units_reused,units_removed,embedded,
             status,duration_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                repo_id.0,
                started_at,
                timestamp,
                stats.changed_sources,
                stats.unchanged_sources,
                stats.deleted_sources,
                stats.units_added,
                stats.units_reused,
                stats.units_removed,
                stats.embedded,
                stats.status.as_str(),
                stats.duration_ms as i64
            ],
        )?;
        Ok(())
    }

    pub fn unit_ids(&self, repo_id: RepoId) -> rusqlite::Result<Vec<i64>> {
        let mut statement = self
            .conn
            .prepare("SELECT id FROM retrieval_units WHERE repo_id=?1 ORDER BY id")?;
        let rows = statement.query_map(params![repo_id.0], |row| row.get(0))?;
        rows.collect()
    }

    pub fn units_missing_vectors(
        &self,
        repo_id: RepoId,
        kind: &str,
        model_version: &str,
    ) -> rusqlite::Result<Vec<(i64, String)>> {
        let column = if kind == "routing" {
            "routing_text"
        } else {
            "evidence_text"
        };
        let sql = format!(
            "SELECT u.id,u.{column} FROM retrieval_units u WHERE u.repo_id=?1
             AND NOT EXISTS (SELECT 1 FROM vectors v WHERE v.unit_id=u.id
             AND v.kind=?2 AND v.model_version=?3) ORDER BY u.id"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params![repo_id.0, kind, model_version], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect()
    }
    pub fn put_vector(
        &self,
        unit_id: i64,
        kind: &str,
        model_version: &str,
        vector: &[f32],
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO vectors(unit_id,kind,model_version,dimensions,vector)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                unit_id,
                kind,
                model_version,
                vector.len() as i64,
                encode_f32(vector)
            ],
        )?;
        Ok(())
    }

    pub fn get_vector(
        &self,
        unit_id: i64,
        kind: &str,
        model_version: &str,
    ) -> rusqlite::Result<Option<Vec<f32>>> {
        self.conn
            .query_row(
                "SELECT vector FROM vectors WHERE unit_id=?1 AND kind=?2 AND model_version=?3",
                params![unit_id, kind, model_version],
                |row| Ok(decode_f32(&row.get::<_, Vec<u8>>(0)?)),
            )
            .optional()
    }

    pub fn fts_search(
        &self,
        repo_id: RepoId,
        column: &str,
        query: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<(i64, f64)>> {
        let Some(expression) = match_expression(column, query) else {
            return Ok(Vec::new());
        };
        let sql = format!(
            "SELECT units_fts.rowid,bm25(units_fts) AS score FROM units_fts
             JOIN retrieval_units u ON u.id=units_fts.rowid
             JOIN sources s ON s.id=u.source_id
             WHERE units_fts MATCH ?1 AND u.repo_id=?2
             ORDER BY score,units_fts.rowid LIMIT ?3"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params![expression, repo_id.0, limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect()
    }

    pub fn top_k_cosine(
        &self,
        repo_id: RepoId,
        kind: &str,
        model_version: &str,
        query: &[f32],
        limit: usize,
    ) -> rusqlite::Result<Vec<(i64, f32)>> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT v.unit_id,vec_distance_cosine(v.vector,?4) AS distance FROM vectors v
             JOIN retrieval_units u ON u.id=v.unit_id
             JOIN sources s ON s.id=u.source_id
             WHERE u.repo_id=?1 AND v.kind=?2
             AND v.model_version=?3 AND v.dimensions=?5
             ORDER BY distance ASC,v.unit_id ASC LIMIT ?6"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(
            params![
                repo_id.0,
                kind,
                model_version,
                encode_f32(query),
                query.len() as i64,
                limit as i64
            ],
            |row| {
                let distance: f32 = row.get(1)?;
                Ok((row.get(0)?, 1.0 - distance))
            },
        )?;
        rows.collect()
    }

    pub fn unit_by_id(&self, unit_id: i64) -> rusqlite::Result<Option<RetrievalUnit>> {
        let row = self
            .conn
            .query_row(
                &format!(
                    "SELECT {UNIT_SELECT} FROM retrieval_units u
                     JOIN sources s ON s.id=u.source_id WHERE u.id=?1"
                ),
                params![unit_id],
                retrieval_unit_from_row,
            )
            .optional()?;
        let Some(mut unit) = row else { return Ok(None) };
        unit.atom_ids = self.unit_atom_ids(unit_id)?;
        Ok(Some(unit))
    }

    /// Units of one source in id order. Atom links are loaded on demand by
    /// `unit_by_id`, so these come back with `atom_ids` empty.
    pub fn unit_by_id_in_repo(
        &self,
        repo_id: RepoId,
        unit_id: i64,
    ) -> rusqlite::Result<Option<RetrievalUnit>> {
        let row = self
            .conn
            .query_row(
                &format!(
                    "SELECT {UNIT_SELECT} FROM retrieval_units u
                     JOIN sources s ON s.id=u.source_id WHERE u.id=?2 AND u.repo_id=?1"
                ),
                params![repo_id.0, unit_id],
                retrieval_unit_from_row,
            )
            .optional()?;
        let Some(mut unit) = row else { return Ok(None) };
        unit.atom_ids = self.unit_atom_ids(unit_id)?;
        Ok(Some(unit))
    }

    pub fn units_for_source(
        &self,
        repo_id: RepoId,
        locator: &str,
    ) -> rusqlite::Result<Vec<RetrievalUnit>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {UNIT_SELECT} FROM retrieval_units u
             JOIN sources s ON s.id=u.source_id WHERE s.repo_id=?1 AND s.locator=?2
             ORDER BY u.id"
        ))?;
        let rows = statement.query_map(params![repo_id.0, locator], retrieval_unit_from_row)?;
        rows.collect()
    }

    fn unit_atom_ids(&self, unit_id: i64) -> rusqlite::Result<Vec<AtomId>> {
        let mut statement = self.conn.prepare(
            "SELECT atom_id FROM retrieval_unit_atoms WHERE unit_id=?1 ORDER BY atom_id",
        )?;
        let rows = statement.query_map(params![unit_id], |row| row.get::<_, i64>(0).map(AtomId))?;
        rows.collect()
    }

    pub fn stats(&self) -> rusqlite::Result<StoreStats> {
        fn count(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
        }
        Ok(StoreStats {
            repositories: count(&self.conn, "repositories")?,
            sources: count(&self.conn, "sources")?,
            units: count(&self.conn, "retrieval_units")?,
            vectors: count(&self.conn, "vectors")?,
            index_runs: count(&self.conn, "index_runs")?,
            last_index_run: None,
        })
    }

    pub fn stats_for_repo(&self, repo_id: RepoId) -> rusqlite::Result<StoreStats> {
        let count = |sql: &str| {
            self.conn
                .query_row(sql, params![repo_id.0], |row| row.get(0))
        };
        let last_index_run = self
            .conn
            .query_row(
                "SELECT finished_at,duration_ms,changed_sources,unchanged_sources,embedded,status
                 FROM index_runs WHERE repo_id=?1 ORDER BY id DESC LIMIT 1",
                params![repo_id.0],
                |row| {
                    Ok(LastIndexRun {
                        finished_at: row.get(0)?,
                        duration_ms: row.get(1)?,
                        changed_sources: row.get(2)?,
                        unchanged_sources: row.get(3)?,
                        embedded: row.get(4)?,
                        status: row.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(StoreStats {
            repositories: count("SELECT count(*) FROM repositories WHERE id=?1")?,
            sources: count("SELECT count(*) FROM sources WHERE repo_id=?1")?,
            units: count("SELECT count(*) FROM retrieval_units WHERE repo_id=?1")?,
            vectors: count(
                "SELECT count(*) FROM vectors v JOIN retrieval_units u ON u.id=v.unit_id WHERE u.repo_id=?1",
            )?,
            index_runs: count("SELECT count(*) FROM index_runs WHERE repo_id=?1")?,
            last_index_run,
        })
    }

    pub fn vector_models(&self, repo_id: RepoId) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut statement = self.conn.prepare(
            "SELECT v.model_version, count(*) FROM vectors v
             JOIN retrieval_units u ON u.id=v.unit_id WHERE u.repo_id=?1
             GROUP BY v.model_version ORDER BY v.model_version",
        )?;
        let rows = statement.query_map(params![repo_id.0], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect()
    }

    pub fn anchors_for_unit(&self, unit_id: i64) -> rusqlite::Result<Vec<(String, String, i64)>> {
        let mut statement = self.conn.prepare(
            "SELECT a.anchor_kind, a.anchor_id, a.relationship, a.confidence_source
             FROM unit_anchors a WHERE a.unit_id=?1
             ORDER BY a.anchor_kind, a.anchor_id",
        )?;
        let rows = statement.query_map(params![unit_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut output = Vec::new();
        for row in rows {
            let (kind, id, relationship, _confidence) = row?;
            output.push((kind, relationship, id));
        }
        Ok(output)
    }

    pub fn anchor_value(
        &self,
        repo_id: RepoId,
        kind: &str,
        anchor_id: i64,
    ) -> rusqlite::Result<Option<String>> {
        let (table, column) = match kind {
            "file" => ("files", "path"),
            "symbol" => ("symbols", "name"),
            "commit" => ("commits", "oid"),
            "session" => ("sessions", "session_id"),
            _ => return Ok(None),
        };
        let sql = format!("SELECT {column} FROM {table} WHERE repo_id=?1 AND id=?2");
        self.conn
            .query_row(&sql, params![repo_id.0, anchor_id], |row| row.get(0))
            .optional()
    }

    pub fn units_for_anchor(
        &self,
        repo_id: RepoId,
        kind: &str,
        value: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<i64>> {
        let (table, column) = match kind {
            "file" => ("files", "path"),
            "symbol" => ("symbols", "name"),
            "commit" => ("commits", "oid"),
            "session" => ("sessions", "session_id"),
            _ => return Ok(Vec::new()),
        };
        let sql = format!(
            "SELECT DISTINCT ua.unit_id FROM unit_anchors ua
             JOIN {table} t ON t.id=ua.anchor_id AND t.repo_id=?1
             JOIN retrieval_units u ON u.id=ua.unit_id
             JOIN sources s ON s.id=u.source_id
             WHERE ua.anchor_kind=?2 AND t.{column}=?3 AND u.repo_id=?1
             ORDER BY ua.unit_id LIMIT ?4"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params![repo_id.0, kind, value, limit as i64], |row| {
            row.get(0)
        })?;
        rows.collect()
    }

    #[cfg(test)]
    fn connection(&self) -> &Connection {
        &self.conn
    }
}

fn repository_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Repository> {
    Ok(Repository {
        id: RepoId(row.get(0)?),
        root_path: row.get(1)?,
        content_version: row.get(2)?,
        metadata: serde_json::from_str(&row.get::<_, String>(3)?)
            .unwrap_or(serde_json::Value::Null),
    })
}

const UNIT_SELECT: &str = "u.id,u.repo_id,u.source_id,s.kind,s.locator,u.kind,u.evidence_text,u.routing_text,u.token_count,u.content_hash,COALESCE(u.timestamp,s.modified_at),u.metadata";

fn retrieval_unit_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RetrievalUnit> {
    Ok(RetrievalUnit {
        id: UnitId(row.get(0)?),
        repo_id: RepoId(row.get(1)?),
        source_id: SourceId(row.get(2)?),
        source_kind: SourceKind::parse(&row.get::<_, String>(3)?).unwrap_or(SourceKind::Text),
        locator: row.get(4)?,
        kind: UnitKind::parse(&row.get::<_, String>(5)?).unwrap_or(UnitKind::Prose),
        evidence_text: row.get(6)?,
        routing_text: row.get(7)?,
        token_count: row.get::<_, i64>(8)? as usize,
        content_hash: row.get(9)?,
        timestamp: row.get(10)?,
        atom_ids: Vec::new(),
        metadata: serde_json::from_str(&row.get::<_, String>(11)?)
            .unwrap_or(serde_json::Value::Null),
    })
}

fn source_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Source> {
    Ok(Source {
        id: SourceId(row.get(0)?),
        repo_id: RepoId(row.get(1)?),
        kind: SourceKind::parse(&row.get::<_, String>(2)?).unwrap_or(SourceKind::Text),
        locator: row.get(3)?,
        content_hash: row.get(4)?,
        modified_at: row.get(5)?,
        metadata: serde_json::from_str(&row.get::<_, String>(6)?)
            .unwrap_or(serde_json::Value::Null),
    })
}

fn match_expression(column: &str, query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\""))
        .collect();
    (!terms.is_empty()).then(|| format!("{column}:({})", terms.join(" OR ")))
}

pub fn encode_f32(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

pub fn decode_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for index in 0..a.len().min(b.len()) {
        dot += a[index] * b[index];
        norm_a += a[index] * a[index];
        norm_b += b[index] * b[index];
    }
    let denominator = norm_a.sqrt() * norm_b.sqrt();
    if denominator <= f32::EPSILON {
        0.0
    } else {
        dot / denominator
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::{EmbedResult, Embedder};
    use crate::ingest::{index_embeddings, index_repository_bounded, LockedError};
    use std::time::Instant;

    fn epoch_secs() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs() as i64)
            .unwrap_or(0)
    }

    fn lease_row(store: &Store, repo: RepoId) -> Option<(String, i64)> {
        store
            .connection()
            .query_row(
                "SELECT owner,expires_at FROM index_leases WHERE repo_id=?1",
                params![repo.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap()
    }

    struct DelayEmbedder {
        version: &'static str,
        delay: Duration,
    }

    impl Embedder for DelayEmbedder {
        fn model_version(&self) -> &str {
            self.version
        }

        fn embed_query(&self, _text: &str) -> EmbedResult<Vec<f32>> {
            Ok(vec![1.0; 8])
        }

        fn embed_documents(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
            std::thread::sleep(self.delay);
            Ok(texts.iter().map(|_| vec![1.0f32; 8]).collect())
        }
    }

    #[test]
    fn migrations_are_idempotent_and_vec_loads() {
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
        assert_eq!(version, 1);
        let busy_timeout: i64 = store
            .connection()
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy_timeout, 5000);
    }

    #[test]
    fn vector_models_lists_per_model_counts() {
        let mut store = Store::open_in_memory().unwrap();
        let repo = RepoId(1);
        store.ensure_repository("/repo").unwrap();
        let unit_ids = store
            .commit_source(SourceIngest {
                repo_id: repo,
                kind: SourceKind::Code,
                locator: "src/a.rs",
                content_hash: "hash",
                modified_at: None,
                metadata: serde_json::json!({}),
                atoms: &[],
                units: &[BuiltUnit {
                    kind: crate::core::UnitKind::Code,
                    evidence_text: "evidence".to_string(),
                    routing_text: "routing".to_string(),
                    token_count: 3,
                    content_hash: "unit-hash".to_string(),
                    atom_indices: Vec::new(),
                    metadata: serde_json::json!({}),
                    anchors: Vec::new(),
                }],
            })
            .unwrap();
        let unit_id = store.units_for_source(repo, "src/a.rs").unwrap()[0].id.0;
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
            store.vector_models(repo).unwrap(),
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
            atom_indices: Vec::new(),
            metadata: serde_json::json!({}),
            anchors: Vec::new(),
        }
    }

    fn commit_routing_unit(store: &mut Store, unit: &BuiltUnit) {
        store
            .commit_source(SourceIngest {
                repo_id: RepoId(1),
                kind: SourceKind::Text,
                locator: "snoop://routed",
                content_hash: "source-hash",
                modified_at: None,
                metadata: serde_json::json!({}),
                atoms: &[],
                units: std::slice::from_ref(unit),
            })
            .unwrap();
    }

    #[test]
    fn reused_unit_with_changed_routing_text_loses_stale_routing_vectors() {
        let mut store = Store::open_in_memory().unwrap();
        let repo = RepoId(1);
        store.ensure_repository("/repo").unwrap();
        commit_routing_unit(&mut store, &routing_unit("old routing text"));
        let unit_id = store.units_for_source(repo, "snoop://routed").unwrap()[0]
            .id
            .0;
        store
            .put_vector(unit_id, "routing", "m1", &[0.1, 0.2])
            .unwrap();
        store
            .put_vector(unit_id, "evidence", "m1", &[0.3, 0.4])
            .unwrap();

        commit_routing_unit(&mut store, &routing_unit("fresh routing text"));
        let reloaded = store.units_for_source(repo, "snoop://routed").unwrap();
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
        let missing = store.units_missing_vectors(repo, "routing", "m1").unwrap();
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
            .units_missing_vectors(repo, "routing", "m1")
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
    fn concurrent_migration_of_a_shared_fresh_database_succeeds() {
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
                "concurrent Store::open must migrate without duplicate-DDL errors"
            );
        }
        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .connection()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn dropped_transaction_writes_nothing() {
        let mut store = Store::open_in_memory().unwrap();
        let transaction = store.conn.transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO repositories(root_path) VALUES ('/tmp/repo')",
                [],
            )
            .unwrap();
        drop(transaction);
        assert_eq!(store.stats().unwrap().repositories, 0);
    }

    #[test]
    fn index_lease_is_exclusive_owner_scoped_and_steals_expired_leases() {
        let store = Store::open_in_memory().unwrap();
        let repo = RepoId(1);
        store.ensure_repository("/repo").unwrap();

        assert!(store.acquire_lease(repo, "indexer-a", 3600).unwrap());
        assert!(
            !store.acquire_lease(repo, "indexer-b", 3600).unwrap(),
            "an unexpired lease is not acquirable"
        );

        store.release_lease(repo, "indexer-b").unwrap();
        assert!(
            !store.acquire_lease(repo, "indexer-c", 3600).unwrap(),
            "release must not remove another owner's lease"
        );

        store
            .connection()
            .execute("UPDATE index_leases SET expires_at=0", [])
            .unwrap();
        assert!(
            store.acquire_lease(repo, "indexer-b", 3600).unwrap(),
            "an expired lease is stolen lazily on acquire"
        );

        store.release_lease(repo, "indexer-b").unwrap();
        assert!(store.acquire_lease(repo, "indexer-c", 3600).unwrap());
    }

    #[test]
    fn record_index_run_writes_status_and_real_started_at() {
        let store = Store::open_in_memory().unwrap();
        let repo = RepoId(1);
        store.ensure_repository("/repo").unwrap();

        store
            .record_index_run(
                repo,
                &IndexRunStats {
                    changed_sources: 2,
                    duration_ms: 5000,
                    ..Default::default()
                },
            )
            .unwrap();
        let (started_at, finished_at, status): (i64, i64, String) = store
            .connection()
            .query_row(
                "SELECT started_at,finished_at,status FROM index_runs WHERE repo_id=?1",
                params![repo.0],
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
            .record_index_run(
                repo,
                &IndexRunStats {
                    status: IndexRunStatus::Timeout,
                    duration_ms: 300_000,
                    ..Default::default()
                },
            )
            .unwrap();
        let run = store
            .stats_for_repo(repo)
            .unwrap()
            .last_index_run
            .expect("latest run is surfaced");
        assert_eq!(run.status, "timeout", "snoop status shows run status");
        assert_eq!(run.duration_ms, 300_000);
    }

    #[test]
    fn renew_lease_renews_only_an_unexpired_owner_lease() {
        let store = Store::open_in_memory().unwrap();
        store.ensure_repository("/repo").unwrap();
        let repo = RepoId(1);

        assert!(store.acquire_lease(repo, "indexer-a", 3).unwrap());
        let e0 = lease_row(&store, repo).unwrap().1;
        std::thread::sleep(Duration::from_millis(1200));

        assert!(
            store.renew_lease(repo, "indexer-a", 3600).unwrap(),
            "the owner renews its own unexpired lease"
        );
        let e1 = lease_row(&store, repo).unwrap().1;
        assert!(
            e1 >= e0 + 3598,
            "renewal pushes expires_at forward by the ttl (e0={e0}, e1={e1})"
        );

        assert!(
            !store.renew_lease(repo, "indexer-b", 3600).unwrap(),
            "a different owner cannot renew the lease"
        );
        assert_eq!(
            lease_row(&store, repo).unwrap().1,
            e1,
            "a failed renewal leaves expires_at untouched"
        );

        // Real wall-clock expiry: an expired lease cannot be renewed.
        store.release_lease(repo, "indexer-a").unwrap();
        assert!(store.acquire_lease(repo, "indexer-c", 1).unwrap());
        std::thread::sleep(Duration::from_millis(1300));
        assert!(
            !store.renew_lease(repo, "indexer-c", 3600).unwrap(),
            "an expired lease cannot be renewed"
        );
        assert!(
            store.acquire_lease(repo, "indexer-d", 60).unwrap(),
            "the expired lease is stolen on acquire"
        );
    }

    #[test]
    fn concurrent_acquire_refuses_live_holder_and_steals_after_real_expiry() {
        let store = Store::open_in_memory().unwrap();
        store.ensure_repository("/repo").unwrap();
        let repo = RepoId(1);

        assert!(store.acquire_lease(repo, "indexer-a", 1).unwrap());
        assert!(
            !store.acquire_lease(repo, "indexer-b", 60).unwrap(),
            "a second indexer is refused while the holder's lease is unexpired"
        );

        std::thread::sleep(Duration::from_millis(1300));
        let (owner, expires) = lease_row(&store, repo).unwrap();
        assert_eq!(owner, "indexer-a");
        assert!(
            expires <= epoch_secs(),
            "the lease has really expired by wall clock (expires={expires}, now={})",
            epoch_secs()
        );

        assert!(
            store.acquire_lease(repo, "indexer-b", 60).unwrap(),
            "the expired lease is stolen by the waiting indexer"
        );
        let (owner, _) = lease_row(&store, repo).unwrap();
        assert_eq!(owner, "indexer-b");
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
        let repository = store.ensure_repository(&root.to_string_lossy()).unwrap();
        assert!(store.acquire_lease(repository.id, "blocker", 3600).unwrap());
        let runs_before = store.stats_for_repo(repository.id).unwrap().index_runs;

        let error = index_repository_bounded(&mut store, &root, None, None).unwrap_err();
        assert!(
            error.is::<LockedError>(),
            "the refusal must be the typed LockedError, got: {error}"
        );
        assert_eq!(
            store.stats_for_repo(repository.id).unwrap().index_runs,
            runs_before,
            "a Locked refusal writes no index_runs row"
        );
        let (owner, _) = lease_row(&store, repository.id).unwrap();
        assert_eq!(owner, "blocker", "the holder's lease is untouched");
    }

    #[test]
    fn index_embeddings_renews_each_embed_request_and_aborts_on_lease_loss() {
        let directory = tempfile::tempdir().unwrap();
        let db = directory.path().join("r1.db");
        let store = Store::open(&db).unwrap();
        let repository = store.ensure_repository("/repo").unwrap();
        let repo = repository.id;

        let source_id: i64 = store
            .connection()
            .query_row(
                "INSERT INTO sources(repo_id,kind,locator,content_hash)
                 VALUES (?1,'file','synthetic://r1','hash-r1') RETURNING id",
                params![repo.0],
                |row| row.get(0),
            )
            .unwrap();
        for index in 0..100 {
            store
                .connection()
                .execute(
                    "INSERT INTO retrieval_units(repo_id,source_id,kind,evidence_text,routing_text,token_count,content_hash)
                     VALUES (?1,?2,'evidence',?3,?3,3,?4)",
                    params![
                        repo.0,
                        source_id,
                        format!("unit {index} evidence"),
                        format!("hash-{index}")
                    ],
                )
                .unwrap();
        }
        assert_eq!(
            store
                .units_missing_vectors(repo, "evidence", "delay-v2")
                .unwrap()
                .len(),
            100,
            "all units are missing vectors for the delay embedder"
        );

        assert!(store.acquire_lease(repo, "index-test-a", 3600).unwrap());

        let worker = std::thread::spawn(move || {
            let embedder = DelayEmbedder {
                version: "delay-v2",
                delay: Duration::from_secs(3),
            };
            index_embeddings(&store, repo, &embedder, None, "index-test-a")
        });

        let observer = Store::open(&db).unwrap();
        let started = Instant::now();
        let mut sighting = None;
        while sighting.is_none() {
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "lease row never appeared"
            );
            sighting = lease_row(&observer, repo);
            std::thread::sleep(Duration::from_millis(100));
        }
        let e0 = sighting.unwrap().1;

        // Renewal cadence: mid multi-chunk embed, expires_at has moved forward.
        std::thread::sleep(Duration::from_secs(4));
        let e1 = lease_row(&observer, repo).unwrap().1;
        assert!(
            e1 >= e0 + 2,
            "expires_at advanced during the embed phase, so renewals run between embed requests (e0={e0}, e1={e1})"
        );

        // A second indexer is refused while the first is alive mid-embed.
        assert!(
            !observer.acquire_lease(repo, "probe", 600).unwrap(),
            "a second indexer is refused while the first holds an unexpired, renewed lease"
        );

        // Scale the TTL horizon: shrink expires_at to now+1 mid-sleep; the next
        // expiry then lands between two embed requests by real wall clock.
        std::thread::sleep(Duration::from_millis(300));
        observer
            .connection()
            .execute(
                "UPDATE index_leases SET expires_at=?1+1 WHERE repo_id=?2",
                params![epoch_secs(), repo.0],
            )
            .unwrap();

        loop {
            let (_, expires) = lease_row(&observer, repo).unwrap();
            if expires <= epoch_secs() {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "lease never reached wall-clock expiry"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            observer.acquire_lease(repo, "takeover", 600).unwrap(),
            "the lease expired by real wall clock and is stolen"
        );

        // The first indexer must abort at its next renewal, not keep writing.
        let error = worker.join().unwrap().unwrap_err();
        assert!(
            error.to_string().contains("lease lost"),
            "the first indexer aborts when it loses the lease, got: {error}"
        );
        let (owner, _) = lease_row(&observer, repo).unwrap();
        assert_eq!(owner, "takeover", "the takeover indexer owns the lease");
    }
}
