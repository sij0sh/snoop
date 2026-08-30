mod anchors;
#[cfg(test)]
mod leases;
mod queries;
mod rows;
mod schema;
#[cfg(test)]
mod tests;

pub use rows::{cosine, decode_f32, encode_f32};

use crate::core::{RepoId, Repository, Source, SourceId, SourceKind};
use rows::{repository_from_row, source_from_row};
use rusqlite::{params, Connection, OptionalExtension};
use schema::{migrate, set_wal_mode};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

pub struct Store {
    pub(super) conn: Connection,
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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
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
}

pub struct SourceIngest<'a> {
    pub repo_id: RepoId,
    pub kind: SourceKind,
    pub locator: &'a str,
    pub content_hash: &'a str,
    pub modified_at: Option<i64>,
    pub metadata: Value,
    pub units: &'a [crate::core::BuiltUnit],
}

pub struct CommitOutcome {
    pub source_id: SourceId,
    pub units_added: usize,
    pub units_reused: usize,
    pub units_removed: usize,
}

#[derive(serde::Serialize)]
pub struct StoreStats {
    pub repositories: i64,
    pub sources: i64,
    pub units: i64,
    pub vectors: i64,
    pub index_runs: i64,
    pub last_index_run: Option<LastIndexRun>,
}

#[derive(Clone, serde::Serialize)]
pub struct LastIndexRun {
    pub finished_at: i64,
    pub duration_ms: i64,
    pub changed_sources: i64,
    pub unchanged_sources: i64,
    pub embedded: i64,
    pub status: String,
}

pub enum IndexRunStatus {
    Ok,
    Timeout,
    Error,
}

impl IndexRunStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Timeout => "timeout",
            Self::Error => "error",
        }
    }
}

pub struct IndexRunStats {
    pub changed_sources: usize,
    pub unchanged_sources: usize,
    pub deleted_sources: usize,
    pub units_added: usize,
    pub units_reused: usize,
    pub units_removed: usize,
    pub embedded: usize,
    pub status: IndexRunStatus,
    pub duration_ms: u64,
}

impl Default for IndexRunStats {
    fn default() -> Self {
        Self {
            changed_sources: 0,
            unchanged_sources: 0,
            deleted_sources: 0,
            units_added: 0,
            units_reused: 0,
            units_removed: 0,
            embedded: 0,
            status: IndexRunStatus::Ok,
            duration_ms: 0,
        }
    }
}

impl Store {
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
        // Anchor links are rebuilt from scratch so reused units never keep
        // stale links from the previous build.
        transaction.execute(
            "DELETE FROM unit_anchors WHERE unit_id IN
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

        for (unit_id, unit) in unit_rows {
            for anchor in &unit.anchors {
                let anchor_id = anchors::ensure_anchor(
                    &transaction,
                    request.repo_id,
                    anchor.kind,
                    &anchor.value,
                )?;
                transaction.execute(
                    "INSERT INTO unit_anchors(unit_id,anchor_id,relationship)
                     VALUES (?1,?2,?3) ON CONFLICT DO NOTHING",
                    params![unit_id, anchor_id, anchor.relationship],
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
                stats.changed_sources as i64,
                stats.unchanged_sources as i64,
                stats.deleted_sources as i64,
                stats.units_added as i64,
                stats.units_reused as i64,
                stats.units_removed as i64,
                stats.embedded as i64,
                stats.status.as_str(),
                stats.duration_ms as i64
            ],
        )?;
        Ok(())
    }
}
