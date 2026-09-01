use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use snoop::core::SourceKind;
use snoop::inference::MockEmbedder;
use snoop::ingest::index_repository_bounded;
use snoop::runtime::{query, QueryChannels, QueryOptions};
use snoop::store::Store;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .env("GIT_AUTHOR_DATE", "2026-08-20T12:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-08-20T12:00:00Z")
        .status()
        .expect("git spawns");
    assert!(status.success(), "git {args:?} failed");
}

const SESSION_LINES: &[&str] = &[
    r#"{"type":"session","version":3,"id":"milestone-c-session","timestamp":"2026-08-21T09:00:00.000Z","cwd":"/tmp/fixture"}"#,
    r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2026-08-21T09:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Investigate the refresh_session rotation order"}]}}"#,
    r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-08-21T09:01:30.000Z","message":{"role":"assistant","content":[{"type":"text","text":"The rotation ordering was deliberate to prevent stale reuse. I checked the retry history and database schema before concluding."},{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"src/auth.rs"}}]}}"#,
];

fn fixture(root: &Path, sessions_root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session() {\n    validate();\n    rotate();\n}\n\nfn validate() {}\n\nfn rotate() {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("docs/auth-plan.md"),
        "# Rotation\n\n`refresh_session` validates the token before rotating.\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "# Fixture\n\nAuth.\n").unwrap();
    git(root, &["init", "--quiet"]);
    git(root, &["add", "."]);
    git(
        root,
        &["commit", "--quiet", "-m", "introduce refresh_session"],
    );

    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session() {\n    validate();\n    rotate();\n    cache();\n}\n\nfn validate() {}\n\nfn rotate() {}\n\nfn cache() {}\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &[
            "commit",
            "--quiet",
            "-m",
            "add cache step to refresh_session",
        ],
    );

    let canonical = root.canonicalize().unwrap();
    let directory = sessions_root.join(snoop::ingest::harness::session_directory_name(
        &canonical.to_string_lossy(),
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let mut lines = vec![format!(
        r#"{{"type":"session","version":3,"id":"milestone-c-session","timestamp":"2026-08-21T09:00:00.000Z","cwd":"{}"}}"#,
        canonical.display()
    )];
    lines.extend(SESSION_LINES[1..].iter().map(|line| line.to_string()));
    std::fs::write(
        directory.join("2026-08-21T09-00-00-000Z_milestone-c-session.jsonl"),
        lines.join("\n") + "\n",
    )
    .unwrap();
}

#[test]
fn four_source_packet_stays_in_budget() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    fixture(directory.path(), sessions_root.path());
    std::env::set_var("SNOOP_SESSIONS_ROOT", sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let report = query(
        &store,
        Some(&embedder),
        "refresh_session rotation order",
        &QueryOptions {
            channels: QueryChannels::for_embedder(Some(&embedder)),
            top_n: 10,
            max_tokens: 2_000,
            diagnostics: false,
            ..QueryOptions::default()
        },
    )
    .unwrap();

    assert!(
        report.packet.token_count <= 2_000,
        "token accounting within budget"
    );
    let kinds: Vec<SourceKind> = report
        .packet
        .items
        .iter()
        .map(|item| item.source_kind)
        .collect();
    assert!(kinds.contains(&SourceKind::Code), "code present");
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, SourceKind::Markdown | SourceKind::Text)),
        "docs present"
    );

    assert!(
        report.packet.items.len() >= 3,
        "packet has multiple distinct items"
    );
    std::env::remove_var("SNOOP_SESSIONS_ROOT");
}

#[test]
fn milestone_c_resumed_work_returns_prior_episode_with_code() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    fixture(directory.path(), sessions_root.path());
    std::env::set_var("SNOOP_SESSIONS_ROOT", sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let report = query(
        &store,
        Some(&embedder),
        "resume work on refresh_session rotation",
        &QueryOptions {
            channels: QueryChannels::for_embedder(Some(&embedder)),
            top_n: 10,
            max_tokens: 3_000,
            diagnostics: false,
            ..QueryOptions::default()
        },
    )
    .unwrap();

    let kinds: Vec<SourceKind> = report
        .packet
        .items
        .iter()
        .map(|item| item.source_kind)
        .collect();
    assert!(
        kinds.contains(&SourceKind::Code),
        "code present alongside prior-agent context: {kinds:?}"
    );
    assert!(
        kinds.contains(&SourceKind::AgentSession),
        "prior episode context present: {kinds:?}"
    );
    let agent = report
        .packet
        .items
        .iter()
        .find(|item| item.source_kind == SourceKind::AgentSession)
        .unwrap();
    assert!(agent
        .evidence_text
        .contains("rotation ordering was deliberate"));
    std::env::remove_var("SNOOP_SESSIONS_ROOT");
}

#[test]
fn full_cli_surface_works_on_the_fixture() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    let sessions_root = tempfile::tempdir().unwrap();
    fixture(&repo, sessions_root.path());

    let db = directory.path().join("index.db");
    let binary = env!("CARGO_BIN_EXE_snoop");
    let run = |args: &[&str], env: &[(&str, &str)]| {
        let mut command = Command::new(binary);
        command.args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{} failed: {}",
            args.first().unwrap_or(&"?"),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let run_with_stderr = |args: &[&str], env: &[(&str, &str)]| {
        let mut command = Command::new(binary);
        command.args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{} failed: {}",
            args.first().unwrap_or(&"?"),
            String::from_utf8_lossy(&output.stderr)
        );
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let repo_arg = repo.to_str().unwrap();
    let db_arg = db.to_str().unwrap();
    let sessions_arg = sessions_root.path().to_str().unwrap();
    let env = [
        ("SNOOP_EMBED_URL", "mock"),
        ("SNOOP_SESSIONS_ROOT", sessions_arg),
    ];

    run(
        &["init", repo_arg, "--db", db_arg],
        &[
            ("SNOOP_EMBED_URL", "mock"),
            ("SNOOP_SESSIONS_ROOT", sessions_arg),
        ],
    );
    run(&["index", repo_arg, "--db", db_arg], &env);
    let status = run(&["status", "--db", db_arg], &env);
    assert!(status.contains("\"sources\""));
    let (query_out, explain_out) = run_with_stderr(
        &[
            "query",
            "refresh_session rotation",
            "--db",
            db_arg,
            "--explain",
        ],
        &env,
    );
    let packet: serde_json::Value = serde_json::from_str(&query_out).unwrap();
    assert!(
        packet["items"]
            .as_array()
            .unwrap()
            .first()
            .is_some_and(|item| item.get("unit_id").is_none()),
        "lean packets carry no unit ids"
    );
    let diagnostics: serde_json::Value = serde_json::from_str(&explain_out).unwrap();
    let first_unit = diagnostics["items"][0]["unit_id"].as_i64().unwrap();
    let inspect_unit = run(
        &["inspect", "unit", &first_unit.to_string(), "--db", db_arg],
        &env,
    );
    let inspected: serde_json::Value = serde_json::from_str(&inspect_unit).unwrap();
    assert!(inspected["unit"]["id"].is_number());
    assert!(inspected["anchors"]
        .as_array()
        .is_some_and(|list| !list.is_empty()));

    let inspect_symbol = run(
        &["inspect", "symbol", "refresh_session", "--db", db_arg],
        &env,
    );
    let symbols: serde_json::Value = serde_json::from_str(&inspect_symbol).unwrap();
    assert!(symbols.as_array().is_some_and(|list| !list.is_empty()));

    let sessions = run(&["sessions", "refresh_session", "--db", db_arg], &env);
    let sessions: serde_json::Value = serde_json::from_str(&sessions).unwrap();
    assert!(
        sessions.as_array().is_some_and(|list| !list.is_empty()),
        "sessions returns prior-agent episodes: {sessions}"
    );

    std::env::remove_var("SNOOP_SESSIONS_ROOT");
}
