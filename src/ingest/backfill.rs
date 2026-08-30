//! Machine-wide embed-backfill admission and budget-bounded embed batches.
//!
//! Audit fix-c4 (AP-2): admission was scoped per-database while the embed
//! endpoint is machine-global. Concurrent backfills queued at one llama.cpp
//! server until batches exceeded the 300 s timeout, and one transient reset
//! silently aborted a whole run. This module adds:
//! - one flock-scoped backfill at a time per machine (`embed-backfill.lock`
//!   under the snoop state dir; `SNOOP_EMBED_BACKFILL_LOCK=0` disables);
//! - per-batch HTTP timeouts bounded by the run's remaining budget;
//! - a small bounded retry for transient embed errors;
//! - a JSON-lines outcome log (`embed-backfill.log`) for done/timed_out/
//!   embed-busy/attempt observability.

use std::error::Error;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::Embedder;

/// Marker error: the run's embed budget ran out. Callers convert this into a
/// clean timed-out outcome (progress stays chunk-granular) instead of a run
/// failure.
#[derive(Debug)]
pub struct BudgetExhausted;

impl std::fmt::Display for BudgetExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "embed budget exhausted")
    }
}

impl Error for BudgetExhausted {}

/// Default per-batch HTTP timeout (matches llama.cpp worst-case batch latency).
const EMBED_HTTP_TIMEOUT: Duration = Duration::from_secs(300);
/// Total attempts per batch: one original plus this many retries of transient
/// errors, each still bounded by the remaining budget.
const EMBED_RETRY_ATTEMPTS: usize = 3;
const EMBED_RETRY_BACKOFF: Duration = Duration::from_millis(250);
const LOCK_POLL: Duration = Duration::from_millis(100);

/// Held-flock handle; dropping it releases the machine-wide backfill slot.
pub struct BackfillGuard {
    _file: Option<std::fs::File>,
}

fn deadline_passed(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

fn state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
        })?;
    let dir = base.join("snoop");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Append one JSON line to the backfill outcome log. Best-effort: logging
/// failures never fail the index run.
pub fn log_event(event: &str, fields: serde_json::Value) {
    let Some(dir) = state_dir() else {
        return;
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0);
    let mut record = serde_json::json!({"ts": timestamp, "event": event, "pid": std::process::id()});
    if let (Some(target), Some(extra)) = (record.as_object_mut(), fields.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("embed-backfill.log"))
    {
        use std::io::Write;
        let _ = file.write_all(format!("{record}\n").as_bytes());
    }
}

/// Acquire the machine-wide backfill slot. Waits for a current holder only
/// within the run's remaining budget; budget exhaustion surfaces as
/// `BudgetExhausted` so the caller can report a clean busy/timeout outcome.
pub fn acquire_backfill_lock(
    deadline: Option<Instant>,
) -> Result<BackfillGuard, Box<dyn Error + Send + Sync>> {
    let disabled = std::env::var_os("SNOOP_EMBED_BACKFILL_LOCK")
        .is_some_and(|value| value == "0")
        || state_dir().is_none();
    if disabled {
        return Ok(BackfillGuard { _file: None });
    }
    let path = state_dir().expect("checked above").join("embed-backfill.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => {
                let wait_ms = started.elapsed().as_millis() as u64;
                if wait_ms > 0 {
                    log_event("lock-wait", serde_json::json!({"wait_ms": wait_ms}));
                }
                return Ok(BackfillGuard { _file: Some(file) });
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if deadline_passed(deadline) {
                    return Err(BudgetExhausted.into());
                }
                std::thread::sleep(LOCK_POLL);
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

fn is_transient(error: &(dyn Error + Send + Sync + 'static)) -> bool {
    error
        .downcast_ref::<crate::inference::EmbedError>()
        .is_none_or(crate::inference::EmbedError::is_transient)
}

/// One embed batch with a budget-bounded HTTP timeout and bounded retries of
/// transient errors (transport failures, timeouts, 5xx/429/408). Permanent
/// errors and a spent retry budget return immediately; budget exhaustion
/// mid-retry returns `BudgetExhausted` so the run can time out cleanly.
pub fn embed_batch_bounded(
    embedder: &dyn Embedder,
    texts: &[String],
    deadline: Option<Instant>,
) -> Result<Vec<Vec<f32>>, Box<dyn Error + Send + Sync>> {
    for attempt in 1..=EMBED_RETRY_ATTEMPTS {
        let remaining = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        if remaining.is_some_and(|remaining| remaining.is_zero()) {
            return Err(BudgetExhausted.into());
        }
        let timeout = remaining
            .map(|remaining| remaining.min(EMBED_HTTP_TIMEOUT))
            .unwrap_or(EMBED_HTTP_TIMEOUT);
        match embedder.embed_documents_bounded(texts, timeout) {
            Ok(vectors) => {
                if attempt > 1 {
                    log_event("retry-ok", serde_json::json!({"attempt": attempt}));
                }
                return Ok(vectors);
            }
            Err(error) => {
                let transient = is_transient(error.as_ref());
                log_event(
                    "embed-error",
                    serde_json::json!({"attempt": attempt, "transient": transient,
                        "error": error.to_string()}),
                );
                if !transient || attempt == EMBED_RETRY_ATTEMPTS {
                    return Err(error);
                }
                let backoff = EMBED_RETRY_BACKOFF * attempt as u32;
                if remaining.is_some_and(|remaining| backoff >= remaining) {
                    return Err(BudgetExhausted.into());
                }
                std::thread::sleep(backoff);
            }
        }
    }
    unreachable!("loop returns on every branch")
}
