//! P3 concurrency proofs (pre-launch indexing plan, section 3.2).
//!
//! 1. Cold-start overlap: two concurrent `ensure` runs admit exactly one indexer.
//! 2. busy_timeout: an ensure blocked by a held write transaction waits, then succeeds.
//! 3. Lease steal: an ensure takes over an expired lease row left by a gone holder.
//! 4. Live holder past TTL: a live holder whose lease lapsed loses it.
//!
//! Within the TTL an ensure reports `locked` (pinned by
//! `cli_ensure_reports_locked_under_a_held_lease` in tests/cli.rs), so the
//! single-active-indexer guarantee is TTL-qualified; production closes that
//! gap by renewing the lease before every embed batch (ingest::index_embeddings).

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
fn ensure_steals_an_expired_lease_row() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    write_repo(&repo, 2);
    let db = directory.path().join("index.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

    let root = scanner::repository_root(&repo)
        .unwrap()
        .to_string_lossy()
        .to_string();
    {
        let store = Store::open(&db).unwrap();
        let repository = store.ensure_repository(&root).unwrap();
        assert!(store.acquire_lease(repository.id, "blocker", 1).unwrap());
    } // holder is gone; only the expired lease row remains

    std::thread::sleep(EXPIRY_SLEEP);

    let (status, report) = run_ensure(binary, &repo, &db);
    assert!(status.success(), "{report}");
    assert_eq!(
        report["status"], "refreshed",
        "ensure steals an expired lease and does the work"
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
    let repository = store.ensure_repository(&root).unwrap();
    assert!(store.acquire_lease(repository.id, "holder", 1).unwrap());
    // The holder process stays alive, but its lease lapses: past the TTL even
    // a live holder loses the lease (within the TTL it would be `locked`, as
    // pinned in tests/cli.rs). Production closes this with per-batch renewal.
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
