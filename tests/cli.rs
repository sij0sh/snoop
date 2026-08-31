use std::process::Command;
#[path = "cli/ensure.rs"]
mod ensure;

#[test]
fn cli_runs_init_index_status_and_query() {
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

    for args in [
        vec!["init", repo.to_str().unwrap(), "--db", db.to_str().unwrap()],
        vec!["index", "--db", db.to_str().unwrap()],
        vec!["status", "--db", db.to_str().unwrap()],
    ] {
        let output = Command::new(binary)
            .args(args)
            .env("SNOOP_EMBED_URL", "mock")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let output = Command::new(binary)
        .args([
            "query",
            "session token",
            "--db",
            db.to_str().unwrap(),
            "--explain",
        ])
        .env("SNOOP_EMBED_URL", "mock")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let packet: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(packet["items"][0]["source_locator"], "README.md");
    let debug: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(!debug["routing_vector"].as_array().unwrap().is_empty());
    assert!(!debug["fused"].as_array().unwrap().is_empty());
    assert!(!debug["items"].as_array().unwrap().is_empty());
    assert!(debug["items"][0]["unit_id"].is_i64());
    let first = &packet["items"][0];
    for field in ["unit_id", "source_slices", "anchors", "selected_because"] {
        assert!(
            first.get(field).is_none(),
            "normal packets stay lean: {field}"
        );
    }
}

#[test]
fn cli_defaults_to_lexical_mode_without_an_embedder() {
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

    for args in [
        vec!["init", repo.to_str().unwrap(), "--db", db.to_str().unwrap()],
        vec!["index", "--db", db.to_str().unwrap()],
    ] {
        let is_index = args[0] == "index";
        let output = Command::new(binary)
            .args(args)
            .env_remove("SNOOP_EMBED_URL")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if is_index {
            let outcome: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(outcome["embedded"], 0, "no embedder: no vectors stored");
        }
    }

    let output = Command::new(binary)
        .args(["status", "--db", db.to_str().unwrap()])
        .env_remove("SNOOP_EMBED_URL")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["retrieval_mode"], "lexical+anchors");
    assert!(status.get("embedding_model").is_none());
    assert_eq!(status["vectors"], 0);

    let output = Command::new(binary)
        .args([
            "query",
            "session token",
            "--db",
            db.to_str().unwrap(),
            "--explain",
        ])
        .env_remove("SNOOP_EMBED_URL")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let packet: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(packet["items"][0]["source_locator"], "README.md");
    let debug: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(debug["routing_vector"].as_array().unwrap().is_empty());
    assert!(!debug["evidence_lexical"].as_array().unwrap().is_empty());
    assert!(!debug["fused"].as_array().unwrap().is_empty());
}

#[test]
fn index_command_refuses_inside_held_lease_without_writing_run_row() {
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
    let index_args = [
        "index",
        repo.to_str().unwrap(),
        "--db",
        db.to_str().unwrap(),
    ];

    let output = Command::new(binary)
        .args(index_args)
        .env("SNOOP_EMBED_URL", "mock")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    {
        use snoop::store::Store;

        let store = Store::open(&db).unwrap();
        let root = snoop::ingest::scanner::repository_root(&repo).unwrap();
        let repository = store
            .repository()
            .unwrap()
            .expect("index auto-initialized the repository");
        assert!(store.acquire_lease("blocker", 3600).unwrap());
        let runs_before = store.stats().unwrap().index_runs;

        let output = Command::new(binary)
            .args(index_args)
            .env("SNOOP_EMBED_URL", "mock")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "index must be refused while another indexer holds the lease"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("locked"),
            "stderr must name the lock: {stderr}"
        );

        let runs_after = store.stats().unwrap().index_runs;
        assert_eq!(
            runs_after, runs_before,
            "a Locked refusal writes no index_runs row"
        );
    }
}

#[test]
fn init_skips_undecodable_source_indexes_the_rest_and_reports_it() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join("good.txt"), "plain utf-8 text for indexing\n").unwrap();
    std::fs::write(repo.join("bad.txt"), b"caf\xe9 latin-1 text\n").unwrap();
    let db = directory.path().join("c4.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

    for run in 1..=2 {
        let output = Command::new(binary)
            .args(["init", repo.to_str().unwrap(), "--db", db.to_str().unwrap()])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "run {run} must not brick the pipeline: {stderr}"
        );
        assert!(
            stderr.contains("bad.txt"),
            "run {run}: the warning must name the locator: {stderr}"
        );
        assert!(
            stdout.contains("1 skipped"),
            "run {run}: init must report the skip: {stdout}"
        );
        assert!(
            stdout.contains("(1 sources"),
            "run {run}: the good source is still indexed: {stdout}"
        );
    }

    let output = Command::new(binary)
        .args([
            "index",
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
        stdout.contains("\"skipped_sources\": 1"),
        "index JSON must report the additive skip: {stdout}"
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
        stdout.contains("\"skipped_sources\": 1"),
        "ensure JSON must report the additive skip: {stdout}"
    );
}

#[test]
fn undecodable_source_is_skipped_and_counted_without_commit() {
    use snoop::ingest::{index_repository_bounded, scanner};
    use snoop::store::Store;

    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(
        repo.join("good.md"),
        "# Auth\n\nRefresh the session token.\n",
    )
    .unwrap();
    std::fs::write(repo.join("bad.md"), b"caf\xe9 latin-1 text\n").unwrap();
    let db = directory.path().join("unit.db");

    let mut store = Store::open(&db).unwrap();
    let root = scanner::repository_root(&repo).unwrap();
    let outcome = index_repository_bounded(&mut store, &root, None, None).unwrap();
    assert_eq!(
        outcome.skipped_sources, 1,
        "the undecodable source is counted"
    );
    assert_eq!(
        outcome.changed_sources, 1,
        "the good source is still committed"
    );

    let repository = store
        .repository()
        .unwrap()
        .expect("the repository row exists");
    assert!(
        store.source_by_locator("bad.md").unwrap().is_none(),
        "the undecodable source is never committed"
    );
    assert!(
        store.source_by_locator("good.md").unwrap().is_some(),
        "the good source is committed"
    );
}

#[test]
fn cli_rejects_a_second_repository_and_keeps_the_first() {
    use snoop::store::Store;
    let directory = tempfile::tempdir().unwrap();
    let repo_a = directory.path().join("repo-a");
    let repo_b = directory.path().join("repo-b");
    std::fs::create_dir(&repo_a).unwrap();
    std::fs::create_dir(&repo_b).unwrap();
    std::fs::write(
        repo_a.join("alpha.rs"),
        "pub fn alpha_only() -> u32 { 1 }\n",
    )
    .unwrap();
    std::fs::write(repo_b.join("beta.rs"), "pub fn beta_only() -> u32 { 2 }\n").unwrap();
    let db = directory.path().join("c5.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

    let output = Command::new(binary)
        .args([
            "init",
            repo_a.to_str().unwrap(),
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

    // The second repository is refused instead of silently sharing the database.
    let output = Command::new(binary)
        .args([
            "index",
            repo_b.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success(), "a second root must be refused");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refusing to index"),
        "the refusal names the bound root: {stderr}"
    );

    // The rejection left the first repository's index intact.
    let store = Store::open(&db).unwrap();
    let bound = store.bind_repository(repo_a.to_str().unwrap()).unwrap();
    assert_eq!(bound.root_path, repo_a.to_str().unwrap());
    let alpha_id = store.units_for_source("alpha.rs").unwrap()[0].id.0;
    drop(store);

    let output = Command::new(binary)
        .args([
            "inspect",
            "unit",
            &alpha_id.to_string(),
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
    assert!(stdout.contains("\"locator\": \"alpha.rs\""), "{stdout}");
}

#[test]
fn cli_index_accepts_canonical_equivalent_paths() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join("a.md"), "# Alpha\n\nCanonical roots only.\n").unwrap();
    let db = directory.path().join("c6.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

    let output = Command::new(binary)
        .args([
            "index",
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

    // A trailing-slash spelling canonicalizes to the same bound root.
    let slashed = format!("{}/", repo.to_str().unwrap());
    let output = Command::new(binary)
        .args(["index", &slashed, "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outcome: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        outcome["changed_sources"], 0,
        "the same canonical root is a no-op reindex"
    );
}

#[test]
fn unreadable_file_skips_and_indexes_the_rest() {
    // Defect-audit c3: a chmod-000 file (vanished/unreadable class) must
    // never abort the run; the rest of the repo still indexes.
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join("good.txt"), "readable content for indexing\n").unwrap();
    let locked = repo.join("secret.txt");
    std::fs::write(&locked, "unreadable content\n").unwrap();
    let mut permissions = std::fs::metadata(&locked).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o000);
    std::fs::set_permissions(&locked, permissions).unwrap();
    let db = directory.path().join("c3.db");
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "an unreadable file must not abort the run: {stderr}"
    );
    assert!(
        stderr.contains("secret.txt"),
        "the warning must name the locator: {stderr}"
    );
    let outcome: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        outcome["outcome"]["skipped_sources"], 1,
        "the skip must be counted: {outcome}"
    );
    assert_eq!(
        outcome["outcome"]["changed_sources"], 1,
        "the readable source is still indexed: {outcome}"
    );
}

#[test]
fn unreadable_directory_skips_and_all_skipped_run_warns_loudly() {
    // Defect-audit c3: an unreadable directory races the walk (entry-level
    // error), and a run that could not read anything must say so loudly.
    let directory = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_snoop");

    // Unreadable directory beside a readable file: the run still succeeds.
    let repo = directory.path().join("partial");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join("good.txt"), "readable content\n").unwrap();
    let locked_dir = repo.join("vault");
    std::fs::create_dir(&locked_dir).unwrap();
    std::fs::write(locked_dir.join("hidden.txt"), "hidden\n").unwrap();
    let mut permissions = std::fs::metadata(&locked_dir).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o000);
    std::fs::set_permissions(&locked_dir, permissions).unwrap();

    let output = Command::new(binary)
        .args([
            "ensure",
            repo.to_str().unwrap(),
            "--db",
            directory.path().join("partial.db").to_str().unwrap(),
        ])
        .env("SNOOP_EMBED_URL", "mock")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "an unreadable directory must not abort the run: {stderr}"
    );
    let outcome: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        outcome["outcome"]["skipped_sources"]
            .as_u64()
            .is_some_and(|n| n >= 1),
        "the unreadable directory must be counted as skipped: {outcome}"
    );
    assert_eq!(
        outcome["outcome"]["changed_sources"], 1,
        "good.txt still indexes"
    );

    // A repo where everything is unreadable: exit 0, but loud on stderr.
    let only = directory.path().join("only");
    std::fs::create_dir(&only).unwrap();
    let locked = only.join("all.txt");
    std::fs::write(&locked, "unreadable\n").unwrap();
    let mut permissions = std::fs::metadata(&locked).unwrap().permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&locked, permissions).unwrap();

    let output = Command::new(binary)
        .args([
            "ensure",
            only.to_str().unwrap(),
            "--db",
            directory.path().join("only.db").to_str().unwrap(),
        ])
        .env("SNOOP_EMBED_URL", "mock")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "an all-skipped run must stay fail-soft: {stderr}"
    );
    assert!(
        stderr.contains("nothing was committed"),
        "an all-skipped run must warn loudly: {stderr}"
    );
}
