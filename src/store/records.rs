use crate::core::{SourceId, SourceKind};
use serde_json::Value;

pub struct SourceIngest<'a> {
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
    pub(super) fn as_str(&self) -> &'static str {
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
