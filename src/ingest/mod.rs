pub mod code;
pub mod git;
pub mod harness;
pub mod markdown;
pub mod scanner;
pub mod text;
pub mod units;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::core::{BuiltUnit, Repository, SourceKind};
use crate::inference::Embedder;
use crate::metadata;
use crate::store::{IndexRunStats, IndexRunStatus, SourceIngest, Store};

/// Identity of the index content layout. Bump whenever a metadata contract
/// in `crate::metadata` changes its persisted shape, so existing databases
/// rebuild every source on the next index run instead of serving old-shape
/// rows (see the upgrade policy in `src/metadata.rs`).
pub const INDEX_FORMAT_VERSION: &str = "phase-13-v1";

/// Operation-owned index lease TTL in seconds.
/// The operation renews the lease before every embed request, so the TTL only
/// has to outlive a single embed batch (observed worst case ~300s); 360s adds
/// a 60s margin so the lease never lapses mid-request.
pub const INDEX_LEASE_TTL_SECS: i64 = 360;

/// Maximum sources per embed request; the lease is renewed before each chunk.
pub const EMBED_CHUNK_LEN: usize = 32;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct IndexOutcome {
    pub changed_sources: usize,
    pub unchanged_sources: usize,
    pub deleted_sources: usize,
    pub units_added: usize,
    pub units_reused: usize,
    pub units_removed: usize,
    pub embedded: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub skipped_sources: usize,
    #[serde(skip_serializing_if = "is_false")]
    pub timed_out: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Returned when another indexer holds an unexpired lease on the repository.
#[derive(Debug)]
pub struct LockedError;

impl std::fmt::Display for LockedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "repository index is locked by another indexer")
    }
}

impl std::error::Error for LockedError {}

fn deadline_passed(deadline: Option<std::time::Instant>) -> bool {
    deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
}

/// One pending source handed from a family loop to the ingest protocol.
struct SourceCandidate {
    kind: SourceKind,
    locator: String,
    content_hash: String,
    modified_at: Option<i64>,
}

/// Family-owned payload for a candidate that passed the skip and deadline
/// checks.
struct Produced {
    metadata: serde_json::Value,
    units: Vec<BuiltUnit>,
}

/// Whether the calling family loop should keep iterating.
enum Step {
    Next,
    DeadlineReached,
}

/// The single owner of the incremental-ingest protocol for one candidate:
/// unchanged-skip, deadline bail, commit, and outcome tally. `produce` runs
/// only after both checks pass; returning `Ok(None)` skips the candidate
/// (for example an undecodable source) without tallying a commit. New
/// source families become candidate producers and must not restate this
/// protocol (guarded by tests/architecture.rs).
fn ingest_candidate(
    store: &mut Store,
    candidate: SourceCandidate,
    force_rebuild: bool,
    deadline: Option<std::time::Instant>,
    outcome: &mut IndexOutcome,
    produce: impl FnOnce(
        &mut IndexOutcome,
    ) -> Result<Option<Produced>, Box<dyn std::error::Error + Send + Sync>>,
) -> Result<Step, Box<dyn std::error::Error + Send + Sync>> {
    if !force_rebuild
        && store
            .source_by_locator(&candidate.locator)?
            .is_some_and(|existing| existing.content_hash == candidate.content_hash)
    {
        outcome.unchanged_sources += 1;
        return Ok(Step::Next);
    }
    if deadline_passed(deadline) {
        outcome.timed_out = true;
        return Ok(Step::DeadlineReached);
    }
    let Some(produced) = produce(outcome)? else {
        return Ok(Step::Next);
    };
    let committed = store.commit_source(SourceIngest {
        kind: candidate.kind,
        locator: &candidate.locator,
        content_hash: &candidate.content_hash,
        modified_at: candidate.modified_at,
        metadata: produced.metadata,
        units: &produced.units,
    })?;
    outcome.changed_sources += 1;
    outcome.units_added += committed.units_added;
    outcome.units_reused += committed.units_reused;
    outcome.units_removed += committed.units_removed;
    Ok(Step::Next)
}

pub fn index_repository_bounded(
    store: &mut Store,
    start: &Path,
    embedder: Option<&dyn Embedder>,
    deadline: Option<std::time::Instant>,
) -> Result<IndexOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let root = scanner::repository_root(start)?;
    let started = std::time::Instant::now();
    let root_string = root.to_string_lossy().to_string();
    let repository = store.bind_repository(&root_string)?;
    let owner = format!(
        "index-{}-{}",
        std::process::id(),
        started.elapsed().as_millis()
    );
    if !store.acquire_lease(&owner, INDEX_LEASE_TTL_SECS)? {
        return Err(LockedError.into());
    }
    let ingested = index_repository_body(store, &root, &repository, embedder, deadline, &owner);
    let _ = store.release_lease(&owner);
    let duration_ms = started.elapsed().as_millis() as u64;
    match ingested {
        Ok(outcome) => {
            let status = if outcome.timed_out {
                IndexRunStatus::Timeout
            } else {
                IndexRunStatus::Ok
            };
            store.record_index_run(&IndexRunStats {
                changed_sources: outcome.changed_sources,
                unchanged_sources: outcome.unchanged_sources,
                deleted_sources: outcome.deleted_sources,
                units_added: outcome.units_added,
                units_reused: outcome.units_reused,
                units_removed: outcome.units_removed,
                embedded: outcome.embedded,
                duration_ms,
                status,
            })?;
            Ok(outcome)
        }
        Err(error) => {
            if error.is::<LockedError>() {
                return Err(error);
            }
            let _ = store.record_index_run(&IndexRunStats {
                duration_ms,
                status: IndexRunStatus::Error,
                ..Default::default()
            });
            Err(error)
        }
    }
}

fn index_repository_body(
    store: &mut Store,
    root: &Path,
    repository: &Repository,
    embedder: Option<&dyn Embedder>,
    deadline: Option<std::time::Instant>,
    lease_owner: &str,
) -> Result<IndexOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let force_rebuild = repository.content_version != INDEX_FORMAT_VERSION;
    let scanned = scanner::scan(&root)?;
    let mut present: HashSet<String> = scanned
        .iter()
        .map(|source| source.locator.clone())
        .collect();
    let mut outcome = IndexOutcome::default();

    if git::is_history_root(&root) {
        let stored_tip = metadata::git_tip::read(&repository.metadata).map(String::from);
        let commits = match (!force_rebuild).then_some(stored_tip.as_deref()).flatten() {
            Some(tip) => git::list_commits_past(&root, git::MAX_COMMITS, tip)?,
            None => git::list_commits(&root, git::MAX_COMMITS)?,
        };
        for locator in store.git_commit_locators()? {
            present.insert(locator);
        }
        let newest_tip = commits.first().map(|commit| commit.oid.clone());
        for commit in commits {
            let locator = format!("git:{}", commit.oid);
            present.insert(locator.clone());
            let step = ingest_candidate(
                store,
                SourceCandidate {
                    kind: SourceKind::GitCommit,
                    locator: locator.clone(),
                    content_hash: commit.content_hash.clone(),
                    modified_at: Some(commit.timestamp),
                },
                force_rebuild,
                deadline,
                &mut outcome,
                |_| {
                    Ok(Some(Produced {
                        metadata: serde_json::json!({"commit": commit.oid.clone()}),
                        units: git::ingest_commit(&root, &commit)?,
                    }))
                },
            )?;
            if matches!(step, Step::DeadlineReached) {
                break;
            }
        }

        if let Some(newest) = newest_tip {
            if !outcome.timed_out {
                store.set_repository_git_tip(&newest)?;
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let mut sessions = harness::discover_sessions(&home, &root)?;
        sessions.sort_by(|a, b| a.path.cmp(&b.path));
        for session in sessions {
            if outcome.timed_out {
                break;
            }
            let locator = harness::session_locator(&session.session_id);
            present.insert(locator.clone());
            let bytes = std::fs::read(&session.path)?;
            if bytes.len() as u64 > harness::MAX_SESSION_BYTES {
                continue;
            }
            let content_hash = blake3::hash(&bytes).to_hex().to_string();
            let step = ingest_candidate(
                store,
                SourceCandidate {
                    kind: SourceKind::AgentSession,
                    locator: locator.clone(),
                    content_hash,
                    modified_at: None,
                },
                force_rebuild,
                deadline,
                &mut outcome,
                |_| {
                    let content = String::from_utf8_lossy(&bytes).into_owned();
                    Ok(Some(Produced {
                        metadata: serde_json::json!({"session": session.session_id.clone()}),
                        units: harness::ingest_pi_session(&content, &session.session_id)?,
                    }))
                },
            )?;
            if matches!(step, Step::DeadlineReached) {
                break;
            }
        }
    }

    for source in &scanned {
        if outcome.timed_out {
            break;
        }
        let step = ingest_candidate(
            store,
            SourceCandidate {
                kind: source.kind,
                locator: source.locator.clone(),
                content_hash: source.content_hash.clone(),
                modified_at: source.modified_at,
            },
            force_rebuild,
            deadline,
            &mut outcome,
            |outcome| {
                let bytes = std::fs::read(&source.path)?;
                if bytes.len() as u64 > scanner::MAX_SOURCE_BYTES {
                    return Err(format!(
                        "source grew beyond size limit during scan: {}",
                        source.locator
                    )
                    .into());
                }
                let read_hash = blake3::hash(&bytes).to_hex().to_string();
                if read_hash != source.content_hash {
                    return Err(
                        format!("source changed during indexing: {}", source.locator).into(),
                    );
                }
                // An undecodable source is skipped, not run-fatal (correction
                // C4). If the source was committed earlier while still
                // decodable, its stale committed version keeps serving: the
                // locator stays in the scanned set, so delete_sources_not_in
                // preserves it while the skip prevents refresh. Skips
                // re-attempt cheaply and idempotently every run; no
                // U+FFFD-poisoned units or fake anchors are ever committed.
                let content = match String::from_utf8(bytes) {
                    Ok(content) => content,
                    Err(_) => {
                        eprintln!(
                            "warning: skipped source that is not valid UTF-8: {}",
                            source.locator
                        );
                        outcome.skipped_sources += 1;
                        return Ok(None);
                    }
                };
                let title = source
                    .path
                    .file_stem()
                    .map(|value| value.to_string_lossy().to_string())
                    .unwrap_or_else(|| source.locator.clone());
                let atoms = match source.kind {
                    SourceKind::Markdown => markdown::parse_markdown(&content, &title).atoms,
                    SourceKind::Text => text::parse_text(&content, &title),
                    SourceKind::Code => code::parse_code(&content, &source.locator)?,
                    SourceKind::GitCommit | SourceKind::AgentSession => unreachable!(),
                };
                Ok(Some(Produced {
                    metadata: serde_json::json!({"path": source.locator.clone()}),
                    units: units::build_units(&atoms, source.kind, &source.locator),
                }))
            },
        )?;
        if matches!(step, Step::DeadlineReached) {
            break;
        }
    }

    // The format version advances only after the complete source rebuild
    // finished inside the deadline. An embedding-only timeout keeps the
    // version current so the next run resumes embedding instead of
    // rebuilding sources.
    if !outcome.timed_out {
        outcome.deleted_sources = store.delete_sources_not_in(&present)?;
        store.set_repository_content_version(INDEX_FORMAT_VERSION)?;
    }
    if !outcome.timed_out {
        if let Some(embedder) = embedder {
            let (embedded, embeddings_timed_out) =
                index_embeddings(store, embedder, deadline, lease_owner)?;
            outcome.embedded = embedded;
            outcome.timed_out = embeddings_timed_out;
        }
    }
    Ok(outcome)
}

pub fn index_embeddings(
    store: &Store,
    embedder: &dyn Embedder,
    deadline: Option<std::time::Instant>,
    lease_owner: &str,
) -> Result<(usize, bool), Box<dyn std::error::Error + Send + Sync>> {
    let mut embedded = 0;
    for kind in ["evidence", "routing"] {
        let missing = store.units_missing_vectors(kind, embedder.model_version())?;
        if missing.is_empty() {
            continue;
        }
        for chunk in missing.chunks(EMBED_CHUNK_LEN) {
            if deadline_passed(deadline) {
                return Ok((embedded, true));
            }
            if !store.renew_lease(lease_owner, INDEX_LEASE_TTL_SECS)? {
                return Err("index lease lost during embedding".into());
            }
            let texts: Vec<String> = chunk.iter().map(|(_, text)| text.clone()).collect();
            let vectors = embedder.embed_documents(&texts)?;
            if vectors.len() != chunk.len() {
                return Err("embedder returned the wrong vector count".into());
            }
            for ((unit_id, _), vector) in chunk.iter().zip(vectors) {
                store.put_vector(*unit_id, kind, embedder.model_version(), &vector)?;
                embedded += 1;
            }
        }
    }
    Ok((embedded, false))
}
