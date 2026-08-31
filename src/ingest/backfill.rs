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

/// Marker error: the index lease was lost (expired and stolen by another
/// holder) while a batch sat in embed retries. Callers map this to a clean
/// "lease lost" run failure so no vectors are written under someone else's
/// lease (defect-audit 20260831023057-8ecdc8ca c2).
#[derive(Debug)]
pub struct LeaseLost;

impl std::fmt::Display for LeaseLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "index lease lost during embed retries")
    }
}

impl Error for LeaseLost {}

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
///
/// `renew_lease` re-arms the caller's index lease before every retry
/// attempt: the caller renews at chunk boundaries, but one batch can sit in
/// retries for ~15 minutes (3 x 300 s HTTP timeout), which is past the
/// 360 s lease TTL. A failed or `Ok(false)` renewal aborts the batch with
/// the renewal error / `LeaseLost` before any late vector write can race a
/// new lease holder (defect-audit 20260831023057-8ecdc8ca c2).
pub fn embed_batch_bounded(
    embedder: &dyn Embedder,
    texts: &[String],
    deadline: Option<Instant>,
    mut renew_lease: impl FnMut() -> Result<bool, Box<dyn Error + Send + Sync>>,
) -> Result<Vec<Vec<f32>>, Box<dyn Error + Send + Sync>> {
    for attempt in 1..=EMBED_RETRY_ATTEMPTS {
        if attempt > 1 && !renew_lease()? {
            // The lease expired and was stolen while we were backoff/sleeping
            // between attempts. Abort before retrying so the late vectors
            // cannot race the new holder (defect-audit c2).
            return Err(LeaseLost.into());
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::{EmbedError, EmbedResult};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Fails the first `transient_failures` calls with a transient error,
    /// then succeeds; counts embed calls.
    struct FlakyEmbedder {
        calls: AtomicUsize,
        transient_failures: usize,
    }

    impl Embedder for FlakyEmbedder {
        fn model_version(&self) -> &str {
            "flaky-v1"
        }

        fn embed_query(&self, _text: &str) -> EmbedResult<Vec<f32>> {
            Ok(vec![0.0])
        }

        fn embed_documents(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
            self.embed_documents_bounded(texts, EMBED_HTTP_TIMEOUT)
        }

        fn embed_documents_bounded(
            &self,
            texts: &[String],
            _timeout: Duration,
        ) -> EmbedResult<Vec<Vec<f32>>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) < self.transient_failures {
                return Err(EmbedError::Transient("flaky transport".into()).into());
            }
            Ok(texts.iter().map(|_| vec![0.0_f32; 2]).collect())
        }
    }

    fn texts() -> Vec<String> {
        vec!["unit-a".to_string()]
    }

    #[test]
    fn renews_lease_before_each_retry_attempt() {
        // c2: attempt 1 is covered by the caller's chunk-boundary renewal;
        // every retry attempt must re-arm the lease first.
        let embedder = FlakyEmbedder { calls: AtomicUsize::new(0), transient_failures: 1 };
        let renewals = AtomicUsize::new(0);
        let vectors = embed_batch_bounded(&embedder, &texts(), None, || {
            renewals.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        })
        .unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 2);
        assert_eq!(renewals.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn first_attempt_success_never_renews() {
        let embedder = FlakyEmbedder { calls: AtomicUsize::new(0), transient_failures: 0 };
        let renewals = AtomicUsize::new(0);
        let vectors = embed_batch_bounded(&embedder, &texts(), None, || {
            renewals.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        })
        .unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);
        assert_eq!(renewals.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn lost_lease_aborts_before_the_retry() {
        // The lease was stolen during backoff: renewal reports false and no
        // second embed call may happen.
        let embedder = FlakyEmbedder { calls: AtomicUsize::new(0), transient_failures: 1 };
        let error = embed_batch_bounded(&embedder, &texts(), None, || Ok(false)).unwrap_err();
        assert!(error.is::<LeaseLost>(), "expected LeaseLost, got {error}");
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn renewal_errors_fail_the_batch() {
        // A broken lease store is a run failure, not a retryable embed error.
        let embedder = FlakyEmbedder { calls: AtomicUsize::new(0), transient_failures: 1 };
        let error =
            embed_batch_bounded(&embedder, &texts(), None, || Err("lease db offline".into()))
                .unwrap_err();
        assert!(error.to_string().contains("lease db offline"));
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);
    }
}