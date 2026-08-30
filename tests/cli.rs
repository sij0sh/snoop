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
        vec![
            "status",
            "--db",
            db.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
        ],
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
            "--repo",
            repo.to_str().unwrap(),
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
        .args([
            "status",
            "--db",
            db.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
        ])
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
            "--repo",
            repo.to_str().unwrap(),
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
            .repository_by_root(&root.to_string_lossy())
            .unwrap()
            .expect("index auto-initialized the repository");
        assert!(store.acquire_lease(repository.id, "blocker", 3600).unwrap());
        let runs_before = store.stats_for_repo(repository.id).unwrap().index_runs;

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

        let runs_after = store.stats_for_repo(repository.id).unwrap().index_runs;
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
        .repository_by_root(&root.to_string_lossy())
        .unwrap()
        .expect("the repository row exists");
    assert!(
        store
            .source_by_locator(repository.id, "bad.md")
            .unwrap()
            .is_none(),
        "the undecodable source is never committed"
    );
    assert!(
        store
            .source_by_locator(repository.id, "good.md")
            .unwrap()
            .is_some(),
        "the good source is committed"
    );
}

#[test]
fn cli_inspect_unit_is_scoped_to_the_selected_repository() {
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

    for repo in [&repo_a, &repo_b] {
        let output = Command::new(binary)
            .args(["init", repo.to_str().unwrap(), "--db", db.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let store = Store::open(&db).unwrap();
    let a = store.ensure_repository(repo_a.to_str().unwrap()).unwrap();
    let b = store.ensure_repository(repo_b.to_str().unwrap()).unwrap();
    let alpha_id = store.units_for_source(a.id, "alpha.rs").unwrap()[0].id.0;
    let beta_id = store.units_for_source(b.id, "beta.rs").unwrap()[0].id.0;

    let output = Command::new(binary)
        .args([
            "inspect",
            "unit",
            &beta_id.to_string(),
            "--db",
            db.to_str().unwrap(),
            "--repo",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a foreign unit id must not print: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("unit {beta_id} not found")),
        "{stderr}"
    );

    let output = Command::new(binary)
        .args([
            "inspect",
            "unit",
            &alpha_id.to_string(),
            "--db",
            db.to_str().unwrap(),
            "--repo",
            repo_b.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "the reverse direction must also refuse: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("unit {alpha_id} not found")),
        "{stderr}"
    );

    let output = Command::new(binary)
        .args([
            "inspect",
            "unit",
            &beta_id.to_string(),
            "--db",
            db.to_str().unwrap(),
            "--repo",
            repo_b.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"locator\": \"beta.rs\""), "{stdout}");
    assert!(
        !stdout.contains("\"anchors\": []"),
        "same-repo inspect must keep anchors: {stdout}"
    );
}
