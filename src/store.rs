mod anchors;
#[cfg(test)]
mod leases;
#[cfg(test)]
mod migration_tests;
mod queries;
mod records;
mod rows;
mod schema;
#[cfg(test)]
mod tests;
mod vec;

pub use records::{
    CommitOutcome, IndexRunStats, IndexRunStatus, LastIndexRun, SourceIngest, StoreStats,
};
pub use rows::{cosine, decode_f32, encode_f32};

use crate::core::{Repository, Source, SourceId};
use rows::{repository_from_row, source_from_row};
use rusqlite::{params, Connection, OptionalExtension};
use schema::{migrate, set_wal_mode};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::Duration;
use vec::register_sqlite_vec;

/// Errors from opening a database or binding its singleton repository root.
#[derive(Debug)]
pub enum StoreOpenError {
    Sqlite(rusqlite::Error),
    MultipleRepositories { repositories: i64 },
    RootMismatch { bound: String, requested: String },
}

impl std::fmt::Display for StoreOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(error) => write!(f, "{error}"),
            Self::MultipleRepositories { repositories } => write!(
                f,
                "database holds {repositories} repositories; snoop keeps one repository \
                 per database — create one database per repository and reindex"
            ),
            Self::RootMismatch { bound, requested } => write!(
                f,
                "database is bound to {bound}; refusing to index {requested} — \
                 use a separate database per repository"
            ),
        }
    }
}

impl std::error::Error for StoreOpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for StoreOpenError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

pub struct Store {
    pub(super) conn: Connection,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreOpenError> {
        register_sqlite_vec()?;
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, StoreOpenError> {
        register_sqlite_vec()?;
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreOpenError> {
        conn.busy_timeout(Duration::from_millis(5000))?;
        set_wal_mode(&conn)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&conn)?;
        Ok(Self { conn })
    }

    /// The singleton repository row, when the database has been indexed.
    pub fn repository(&self) -> rusqlite::Result<Option<Repository>> {
        self.conn
            .query_row(
                "SELECT root_path, content_version, metadata FROM repository",
                [],
                repository_from_row,
            )
            .optional()
    }

    /// Binds the database to one canonical repository root.
    /// A second root is rejected instead of silently chosen.
    pub fn bind_repository(&self, root_path: &str) -> Result<Repository, StoreOpenError> {
        match self.repository()? {
            Some(bound) if bound.root_path == root_path => Ok(bound),
            Some(bound) => Err(StoreOpenError::RootMismatch {
                bound: bound.root_path,
                requested: root_path.to_string(),
            }),
            None => {
                self.conn.execute(
                    "INSERT INTO repository(root_path) VALUES (?1)",
                    params![root_path],
                )?;
                Ok(self.repository()?.expect("row was just inserted"))
            }
        }
    }

    pub fn set_repository_content_version(&self, version: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("UPDATE repository SET content_version=?1", params![version])?;
        Ok(())
    }

    pub fn set_repository_git_tip(&self, tip: &str) -> rusqlite::Result<()> {
        let mut metadata = self
            .conn
            .query_row("SELECT metadata FROM repository", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
            .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        crate::metadata::git_tip::set(&mut metadata, tip);
        self.conn.execute(
            "UPDATE repository SET metadata=?1",
            params![metadata.to_string()],
        )?;
        Ok(())
    }

    pub fn git_commit_locators(&self) -> rusqlite::Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT locator FROM sources WHERE kind='git_commit'")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    pub fn source_by_locator(&self, locator: &str) -> rusqlite::Result<Option<Source>> {
        self.conn
            .query_row(
                "SELECT id, kind, locator, content_hash, modified_at, metadata
                 FROM sources WHERE locator=?1",
                params![locator],
                source_from_row,
            )
            .optional()
    }

    pub fn source_locators(&self) -> rusqlite::Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT locator FROM sources ORDER BY locator")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    pub fn delete_sources_not_in(&self, present: &HashSet<String>) -> rusqlite::Result<usize> {
        let mut deleted = 0;
        for locator in self.source_locators()? {
            if !present.contains(&locator) {
                deleted += self
                    .conn
                    .execute("DELETE FROM sources WHERE locator=?1", params![locator])?;
            }
        }
        Ok(deleted)
    }

    /// Acquires the database-scoped index lease; lazily steals an expired one.
    /// Returns false when another indexer holds an unexpired lease.
    pub fn acquire_lease(&self, owner: &str, ttl_secs: i64) -> rusqlite::Result<bool> {
        let timestamp = now();
        let changed = self.conn.execute(
            "INSERT INTO index_leases(id,owner,expires_at) VALUES (1,?1,?2)
             ON CONFLICT(id) DO UPDATE SET owner=excluded.owner, expires_at=excluded.expires_at
             WHERE index_leases.expires_at<=?3",
            params![owner, timestamp + ttl_secs, timestamp],
        )?;
        Ok(changed > 0)
    }

    /// Releases the lease only when the caller owns it.
    pub fn release_lease(&self, owner: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM index_leases WHERE id=1 AND owner=?1",
            params![owner],
        )?;
        Ok(())
    }

    /// Renews the lease only when the caller owns it and it has not expired.
    /// Returns false when the lease is missing, stolen, or expired.
    pub fn renew_lease(&self, owner: &str, ttl_secs: i64) -> rusqlite::Result<bool> {
        let timestamp = now();
        let changed = self.conn.execute(
            "UPDATE index_leases SET expires_at=?2
             WHERE id=1 AND owner=?1 AND expires_at>?3",
            params![owner, timestamp + ttl_secs, timestamp],
        )?;
        Ok(changed > 0)
    }
}

impl Store {
    pub fn commit_source(&mut self, request: SourceIngest<'_>) -> rusqlite::Result<CommitOutcome> {
        let transaction = self.conn.transaction()?;
        let source_id: i64 = transaction.query_row(
            "INSERT INTO sources(kind, locator, content_hash, modified_at, metadata)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(locator) DO UPDATE SET
               kind=excluded.kind, content_hash=excluded.content_hash,
               modified_at=excluded.modified_at, metadata=excluded.metadata
             RETURNING id",
            params![
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

        // Finding 4 (audit 20260830195149-6f1a96a5): relocate reused rows
        // through an id map instead of a linear scan per reused unit (was
        // Theta(U_reused x U_old), exact U(U+1)/2 dense scans).
        let old_row_by_id: HashMap<i64, usize> = old_units
            .iter()
            .enumerate()
            .map(|(index, (id, _, _, _))| (*id, index))
            .collect();
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
                let old_row_index = *old_row_by_id
                    .get(&id)
                    .expect("reused unit id comes from the previous rows");
                let old_row = &old_units[old_row_index];
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
                        crate::metadata::timestamp::read(&unit.metadata)
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
                    "INSERT INTO retrieval_units(source_id,kind,evidence_text,routing_text,
                     token_count,content_hash,metadata,timestamp) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                     RETURNING id",
                    params![
                        source_id,
                        unit.kind.as_str(),
                        unit.evidence_text,
                        unit.routing_text,
                        unit.token_count,
                        unit.content_hash,
                        unit.metadata.to_string(),
                        crate::metadata::timestamp::read(&unit.metadata)
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
                let anchor_id = anchors::ensure_anchor(&transaction, anchor.kind, &anchor.value)?;
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

    pub fn record_index_run(&self, stats: &IndexRunStats) -> rusqlite::Result<()> {
        let timestamp = now();
        // started_at is real: the runner reports elapsed duration, so start = finish - duration.
        let started_at = timestamp.saturating_sub((stats.duration_ms as i64) / 1000);
        self.conn.execute(
            "INSERT INTO index_runs(started_at,finished_at,changed_sources,
             unchanged_sources,deleted_sources,units_added,units_reused,units_removed,embedded,
             status,duration_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
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
