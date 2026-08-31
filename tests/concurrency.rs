//! P3 concurrency proofs (pre-launch indexing plan, section 3.2).
//!
//! 1. Cold-start overlap: two concurrent `ensure` runs admit exactly one indexer.
//! 2. busy_timeout: an ensure blocked by a held write transaction waits, then succeeds.
//! 3. Lease steal: an ensure takes over an expired lease row past the TTL,
//!    whether its holder process is live or gone (row state is all the code
//!    can see; pinned by `ensure_steals_a_live_holders_expired_lease`).
//!
//! Within the TTL an ensure reports `locked` (pinned by
//! `cli_ensure_reports_locked_under_a_held_lease` in tests/cli.rs), so the
//! single-active-indexer guarantee is TTL-qualified; production closes that
//! gap by renewing the lease before every embed batch and before every
//! in-batch retry (ingest::index_embeddings, backfill::embed_batch_bounded).
//! The `embed_lease_choreography` tests below pin the renew-and-abort
//! contract at the library level.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use snoop::ingest::scanner;
use snoop::store::Store;

/// Overlap fixture size: ~3 embed batches at EMBED_CHUNK_LEN=32, keeping the
/// winner's lease window wider than the contender's process startup.
const OVERLAP_FILES: usize = 80;
const HOLD: Duration = Duration::from_millis(1200);
const EXPIRY_SLEEP: Duration = Duration::from_millis(1300);
const LEASE_GATE: Duration = Duration::from_secs(10);

fn write_repo(repo: &Path, files: usize) {
    std::fs::create_dir(repo).unwrap();
    for i in 0..files {
        std::fs::write(
            repo.join(format!("note_{i:03}.md")),
            format!("# Note {i}\n\nRefresh the session token for service {i}.\n"),
        )
        .unwrap();
    }
}

fn run_ensure(
    binary: &str,
    repo: &Path,
    db: &Path,
) -> (std::process::ExitStatus, serde_json::Value) {
    let output = Command::new(binary)
        .args([
            "ensure",
            repo.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .env("SNOOP_EMBED_URL", "mock")
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    (output.status, report)
}

/// Polls for any lease row so the contender starts while the winner holds it.
/// Uses a raw reader connection: WAL readers never block the writer, and no
/// public lease-read API exists (store.rs exposes only acquire/release/renew).
fn wait_for_lease(db: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let conn = Connection::open(db).unwrap();
    while Instant::now() < deadline {
        let leases: i64 = conn
            .query_row("SELECT COUNT(*) FROM index_leases", [], |row| row.get(0))
            .unwrap_or(0);
        if leases > 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn cold_start_overlap_admits_exactly_one_indexer() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    write_repo(&repo, OVERLAP_FILES);
    let db = directory.path().join("index.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

    let winner = Command::new(binary)
        .args([
            "ensure",
            repo.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .env("SNOOP_EMBED_URL", "mock")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Gate on the winner's lease row, then freeze the winner mid-index so
    // its lease stays live until the contender has been refused. Indexing
    // is fast enough with mock embeddings that a pure timing race is flaky.
    assert!(
        wait_for_lease(&db, LEASE_GATE),
        "first ensure never acquired the index lease"
    );
    Command::new("kill")
        .args(["-STOP", &winner.id().to_string()])
        .output()
        .unwrap();

    let contender = run_ensure(binary, &repo, &db);
    assert!(
        contender.0.success(),
        "locked is fail-soft: {}",
        contender.1
    );
    assert_eq!(
        contender.1["status"], "locked",
        "the contender must see the winner's live lease"
    );

    Command::new("kill")
        .args(["-CONT", &winner.id().to_string()])
        .output()
        .unwrap();

    let winner_out = winner.wait_with_output().unwrap();
    assert!(
        winner_out.status.success(),
        "{}",
        String::from_utf8_lossy(&winner_out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&winner_out.stdout).unwrap();
    assert_eq!(
        report["status"], "refreshed",
        "exactly one of the overlapping ensures may refresh"
    );

    let (status, report) = run_ensure(binary, &repo, &db);
    assert!(status.success(), "{report}");
    assert_eq!(
        report["status"], "up-to-date",
        "the winner's commit is complete"
    );
}

#[test]
fn ensure_waits_out_a_held_write_transaction_then_succeeds() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    write_repo(&repo, 1);
    let db = directory.path().join("index.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

    // Warm the database (WAL mode set) so the holder only contends writers.
    let (status, report) = run_ensure(binary, &repo, &db);
    assert!(status.success(), "{report}");
    assert_eq!(report["status"], "refreshed");
    std::fs::write(
        repo.join("note_pending.md"),
        "# Pending\n\nRefresh the session token again.\n",
    )
    .unwrap();

    // Holder takes the writer lock in-process; the channel fires only after
    // BEGIN IMMEDIATE succeeds, so the ensure below deterministically waits.
    let db_path = db.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let conn = Connection::open(&db_path).unwrap();
        conn.busy_timeout(Duration::from_millis(5000)).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        tx.send(()).unwrap();
        std::thread::sleep(HOLD);
        conn.execute_batch("COMMIT").unwrap();
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let started = Instant::now();
    let (status, report) = run_ensure(binary, &repo, &db);
    let elapsed = started.elapsed();
    assert!(status.success(), "{report}");
    assert_eq!(report["status"], "refreshed");
    assert!(
        elapsed >= HOLD,
        "ensure finished in {elapsed:?} without waiting out the held write transaction"
    );

    holder.join().unwrap();
    let (status, report) = run_ensure(binary, &repo, &db);
    assert!(status.success(), "{report}");
    assert_eq!(
        report["status"], "up-to-date",
        "the waited-out run committed"
    );
}

#[test]
fn ensure_steals_a_live_holders_expired_lease() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    write_repo(&repo, 2);
    let db = directory.path().join("index.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

    let root = scanner::repository_root(&repo)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let store = Store::open(&db).unwrap();
    let repository = store.bind_repository(&root).unwrap();
    assert!(store.acquire_lease("holder", 1).unwrap());
    // The holder process stays alive, but its lease lapses: past the TTL even
    // a live holder loses the lease (within the TTL it would be `locked`, as
    // pinned in tests/cli.rs). Production closes this with per-batch and
    // per-retry lease renewal plus abort-on-loss.
    std::thread::sleep(EXPIRY_SLEEP);

    let (status, report) = run_ensure(binary, &repo, &db);
    assert!(status.success(), "{report}");
    assert_eq!(
        report["status"], "refreshed",
        "a live holder past the TTL no longer blocks indexing"
    );

    let conn = Connection::open(&db).unwrap();
    let leases: i64 = conn
        .query_row("SELECT COUNT(*) FROM index_leases", [], |row| row.get(0))
        .unwrap();
    assert_eq!(leases, 0, "ensure released the stolen lease");
    drop(store);

    let (status, report) = run_ensure(binary, &repo, &db);
    assert!(status.success(), "{report}");
    assert_eq!(report["status"], "up-to-date", "database stayed consistent");
}

// ---------------------------------------------------------------------------
// Embed-lease choreography (defect-audit 20260831023057-8ecdc8ca c2).
//
// These pin the library-level contract behind the CLI steal tests above:
// the embed retry loop renews the index lease before every attempt and must
// abort with a clean "lease lost" failure instead of writing vectors under
// a lapsed or stolen lease. Lease expiry is forced with direct SQL so the
// choreography has no wall-clock races.

mod embed_lease_choreography {
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Mutex;
    use std::time::Duration;

    use rusqlite::Connection;
    use snoop::core::{hash_segments, AnchorKind, BuiltAnchor, BuiltUnit, SourceKind, UnitKind};
    use snoop::inference::{EmbedError, EmbedResult, Embedder};
    use snoop::ingest::index_embeddings;
    use snoop::store::{SourceIngest, Store};

    /// One scripted embed attempt.
    enum Step {
        /// Immediate transient failure.
        Transient,
        /// Transient failure, delayed until the gate receives a value.
        StallThenTransient(Receiver<()>),
        /// Success, delayed until the gate receives a value.
        StallThenSucceed(Receiver<()>),
    }

    struct ScriptedEmbedder {
        script: Mutex<VecDeque<Step>>,
        events: Sender<&'static str>,
    }

    impl ScriptedEmbedder {
        fn new(steps: Vec<Step>) -> (Self, Receiver<&'static str>) {
            let (events, receiver) = channel();
            (
                Self {
                    script: Mutex::new(steps.into()),
                    events,
                },
                receiver,
            )
        }

        fn gate() -> (Sender<()>, Receiver<()>) {
            channel()
        }
    }

    impl Embedder for ScriptedEmbedder {
        fn model_version(&self) -> &str {
            "scripted-v1"
        }

        fn embed_query(&self, _text: &str) -> EmbedResult<Vec<f32>> {
            Ok(vec![0.0; 2])
        }

        fn embed_documents(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
            self.embed_documents_bounded(texts, Duration::from_secs(1))
        }

        fn embed_documents_bounded(
            &self,
            texts: &[String],
            _timeout: Duration,
        ) -> EmbedResult<Vec<Vec<f32>>> {
            self.events.send("embed").unwrap();
            match self.script.lock().unwrap().pop_front() {
                Some(Step::Transient) => {
                    Err(EmbedError::Transient("scripted transient".into()).into())
                }
                Some(Step::StallThenTransient(gate)) => {
                    gate.recv().unwrap();
                    Err(EmbedError::Transient("scripted transient".into()).into())
                }
                Some(Step::StallThenSucceed(gate)) => {
                    gate.recv().unwrap();
                    Ok(texts.iter().map(|_| vec![0.0_f32; 2]).collect())
                }
                None => panic!("scripted embedder ran out of steps"),
            }
        }
    }

    /// File-backed store with one committed unit still missing vectors.
    fn committed_store(db: &Path) -> Store {
        let mut store = Store::open(db).unwrap();
        store.bind_repository("/repo").unwrap();
        let unit = BuiltUnit {
            kind: UnitKind::Prose,
            evidence_text: "unit body".to_string(),
            routing_text: "unit body".to_string(),
            token_count: 3,
            content_hash: hash_segments(&["unit body"]),
            metadata: serde_json::json!({}),
            anchors: vec![BuiltAnchor {
                kind: AnchorKind::File,
                value: "/repo/note.md".to_string(),
                relationship: "touched".to_string(),
            }],
        };
        store
            .commit_source(SourceIngest {
                kind: SourceKind::Markdown,
                locator: "/repo/note.md",
                content_hash: "source-hash",
                modified_at: None,
                metadata: serde_json::json!({}),
                units: &[unit],
            })
            .unwrap();
        store
    }

    /// Force the lease to lapse now: the holder stopped renewing, exactly as
    /// if its TTL had elapsed mid-embed.
    fn expire_lease(db: &Path) {
        Connection::open(db)
            .unwrap()
            .execute(
                "UPDATE index_leases SET expires_at = expires_at - 999999",
                [],
            )
            .unwrap();
    }

    fn pending_vectors(db: &Path) -> usize {
        Store::open(db)
            .unwrap()
            .units_missing_vectors_page("evidence", "scripted-v1", 0, 32)
            .unwrap()
            .len()
    }

    fn wait_event(receiver: &Receiver<&'static str>) {
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(10)).unwrap(),
            "embed"
        );
    }

    #[test]
    fn lapsed_lease_aborts_the_retry_before_vectors_are_written() {
        let directory = tempfile::tempdir().unwrap();
        let db = directory.path().join("index.db");
        let mut store = committed_store(&db);
        assert!(store.acquire_lease("run1", 3600).unwrap());

        // Attempt 1 stalls mid-embed (slow HTTP); while it is stalled the
        // lease lapses and nobody steals it.
        let (release, gate) = ScriptedEmbedder::gate();
        let (embedder, events) = ScriptedEmbedder::new(vec![Step::StallThenTransient(gate)]);
        let worker = {
            let db = db.to_path_buf();
            std::thread::spawn(move || {
                let store = Store::open(&db).unwrap();
                index_embeddings(&store, &embedder, None, "run1")
            })
        };
        wait_event(&events); // attempt 1 is now in flight
        expire_lease(&db);
        release.send(()).unwrap(); // attempt 1 fails transiently

        // Without the in-batch renewal the retry would run under a dead
        // lease and write vectors; with it, the renewal reports the lapse
        // and the run aborts cleanly before any write.
        let result = worker.join().unwrap();
        let error = result.err().expect("run must abort on the lapsed lease");
        assert!(error.to_string().contains("index lease lost"), "{error}");
        assert_eq!(
            pending_vectors(&db),
            1,
            "no vectors may be written after the lease lapsed"
        );
        drop(store);
    }

    #[test]
    fn stolen_lease_mid_embed_steals_from_a_stalled_holder() {
        let directory = tempfile::tempdir().unwrap();
        let db = directory.path().join("index.db");
        let mut store = committed_store(&db);
        assert!(store.acquire_lease("run1", 3600).unwrap());

        // Attempt 1 stalls mid-embed; the lease lapses and a contender
        // steals it (steal-on-stall must keep working mid-batch).
        let (release, gate) = ScriptedEmbedder::gate();
        let (embedder, events) = ScriptedEmbedder::new(vec![Step::StallThenTransient(gate)]);
        let worker = {
            let db = db.to_path_buf();
            std::thread::spawn(move || {
                let store = Store::open(&db).unwrap();
                index_embeddings(&store, &embedder, None, "run1")
            })
        };
        wait_event(&events); // attempt 1 is now in flight
        expire_lease(&db);
        let contender = Store::open(&db).unwrap();
        assert!(
            contender.acquire_lease("stealer", 3600).unwrap(),
            "a stalled holder's lapsed lease must be stealable"
        );
        release.send(()).unwrap();

        // run1's pre-retry renewal now fails: the lease belongs to stealer.
        let result = worker.join().unwrap();
        let error = result
            .err()
            .expect("stolen lease must abort the old holder");
        assert!(error.to_string().contains("index lease lost"), "{error}");
        assert_eq!(
            pending_vectors(&db),
            1,
            "run1 must not write vectors under stealer's lease"
        );
        let owner: String = Connection::open(&db)
            .unwrap()
            .query_row("SELECT owner FROM index_leases", [], |row| row.get(0))
            .unwrap();
        assert_eq!(owner, "stealer", "the stealer's lease must stand");
        drop(store);
    }
}
