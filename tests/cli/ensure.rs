//! `snoop ensure` CLI behavior: refresh, timeout, locking, and backfill reports.

use super::*;

#[test]
fn cli_ensure_refreshes_then_reports_up_to_date() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(
        repo.join("README.md"),
        "# Auth\n\nRefresh the session token.\n",
    )
    .unwrap();
    let db = directory.path().join("index.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

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
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "refreshed");
    assert_eq!(report["outcome"]["changed_sources"], 1);
    assert_eq!(report["outcome"]["embedded"], 2);

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
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "up-to-date");
    assert_eq!(report["outcome"]["changed_sources"], 0);
    assert_eq!(report["outcome"]["embedded"], 0);

    let output = Command::new(binary)
        .args([
            "status",
            "--db",
            db.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
        ])
        .env("SNOOP_EMBED_URL", "mock")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["last_index_run"]["status"], "ok");
}

#[test]
fn cli_ensure_reports_timeout_with_a_zero_budget_and_self_heals() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(
        repo.join("README.md"),
        "# Auth\n\nRefresh the session token.\n",
    )
    .unwrap();
    let db = directory.path().join("index.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

    let output = Command::new(binary)
        .args([
            "ensure",
            repo.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
            "--timeout",
            "0",
        ])
        .env("SNOOP_EMBED_URL", "mock")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "timeout");
    assert!(report.get("outcome").is_none());

    let output = Command::new(binary)
        .args([
            "status",
            "--db",
            db.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
        ])
        .env("SNOOP_EMBED_URL", "mock")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        status["last_index_run"]["status"], "timeout",
        "a timed-out run must never look fresh"
    );

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
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "refreshed", "timeout self-heals on rerun");
}

#[test]
fn cli_ensure_reports_locked_under_a_held_lease() {
    use snoop::store::Store;

    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(
        repo.join("README.md"),
        "# Auth\n\nRefresh the session token.\n",
    )
    .unwrap();
    let db = directory.path().join("index.db");
    let binary = env!("CARGO_BIN_EXE_snoop");
    let ensure_args = [
        "ensure",
        repo.to_str().unwrap(),
        "--db",
        db.to_str().unwrap(),
    ];

    let output = Command::new(binary)
        .args(ensure_args)
        .env("SNOOP_EMBED_URL", "mock")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let root = snoop::ingest::scanner::repository_root(&repo).unwrap();
    let store = Store::open(&db).unwrap();
    let repository = store
        .repository_by_root(&root.to_string_lossy())
        .unwrap()
        .expect("ensure auto-initialized the repository");
    assert!(store.acquire_lease(repository.id, "blocker", 3600).unwrap());

    let output = Command::new(binary)
        .args(ensure_args)
        .env("SNOOP_EMBED_URL", "mock")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "locked is fail-soft: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "locked");

    store.release_lease(repository.id, "blocker").unwrap();
    drop(store);
    let output = Command::new(binary)
        .args(ensure_args)
        .env("SNOOP_EMBED_URL", "mock")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["status"], "up-to-date",
        "ensure runs after the blocker releases"
    );
}

#[test]
fn cli_ensure_reports_refreshed_on_delete_only_run() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join("a.txt"), "alpha source text\n").unwrap();
    std::fs::write(repo.join("b.txt"), "bravo source text\n").unwrap();
    let db = directory.path().join("c3.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

    let output = Command::new(binary)
        .args(["init", repo.to_str().unwrap(), "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    std::fs::remove_file(repo.join("b.txt")).unwrap();
    let output = Command::new(binary)
        .args([
            "ensure",
            repo.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"status\": \"refreshed\""),
        "a delete-only run mutated the index and must not say up-to-date: {stdout}"
    );
    assert!(
        stdout.contains("\"deleted_sources\": 1"),
        "the same report records the deletion: {stdout}"
    );

    let output = Command::new(binary)
        .args([
            "ensure",
            repo.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"status\": \"up-to-date\""),
        "an unchanged run still reports up-to-date: {stdout}"
    );
}

#[test]
fn cli_ensure_reports_refreshed_on_embed_only_backfill() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(
        repo.join("README.md"),
        "# Auth\n\nRefresh the session token.\n",
    )
    .unwrap();
    let db = directory.path().join("lead3.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

    let output = Command::new(binary)
        .args(["init", repo.to_str().unwrap(), "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

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
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"status\": \"refreshed\""),
        "an embed-only backfill mutated the index and must not say up-to-date: {stdout}"
    );
    assert!(
        stdout.contains("\"changed_sources\": 0") && stdout.contains("\"embedded\": "),
        "the mutation was embedding only: {stdout}"
    );

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
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"status\": \"up-to-date\""),
        "a fully embedded, unchanged run still reports up-to-date: {stdout}"
    );
}
