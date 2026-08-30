use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use snoop::core::SourceKind;
use snoop::inference::MockEmbedder;
use snoop::ingest::index_repository_bounded;
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

fn count_kind(store: &Store, kind: SourceKind) -> usize {
    store
        .unit_ids()
        .unwrap()
        .into_iter()
        .filter_map(|id| store.unit_by_id(id).unwrap())
        .filter(|unit| unit.source_kind == kind)
        .count()
}

#[test]
fn git_incremental_indexes_only_new_commits() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src")).unwrap();
    std::fs::write(
        directory.path().join("src/auth.rs"),
        "pub fn refresh_session() {}\n",
    )
    .unwrap();
    git(directory.path(), &["init", "--quiet"]);
    git(directory.path(), &["add", "."]);
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "first: introduce refresh"],
    );

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let first =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(count_kind(&store, SourceKind::GitCommit), 1);
    let git_sources_after_first = store.git_commit_locators().unwrap();
    assert_eq!(git_sources_after_first.len(), 1);

    std::fs::write(
        directory.path().join("src/rotate.rs"),
        "pub fn rotate_token() {}\n",
    )
    .unwrap();
    git(directory.path(), &["add", "."]);
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "second: token rotation"],
    );

    let second =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(
        count_kind(&store, SourceKind::GitCommit),
        2,
        "stored first commit must survive and the new commit must be indexed"
    );
    assert!(
        second.units_reused > 0 || second.unchanged_sources > 0,
        "incremental run must reuse stored work: {:?}",
        second
    );
    assert_eq!(store.git_commit_locators().unwrap().len(), 2);
}

#[test]
fn git_force_rebuild_walks_all_commits() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src")).unwrap();
    std::fs::write(
        directory.path().join("src/auth.rs"),
        "pub fn refresh_session() {}\n",
    )
    .unwrap();
    git(directory.path(), &["init", "--quiet"]);
    git(directory.path(), &["add", "."]);
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "first: introduce refresh"],
    );
    std::fs::write(
        directory.path().join("src/rotate.rs"),
        "pub fn rotate_token() {}\n",
    )
    .unwrap();
    git(directory.path(), &["add", "."]);
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "second: token rotation"],
    );

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let first =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(count_kind(&store, SourceKind::GitCommit), 2);

    store
        .set_repository_content_version("stale-version")
        .unwrap();
    let rebuilt =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(
        count_kind(&store, SourceKind::GitCommit),
        2,
        "force rebuild must reprocess every stored commit"
    );
    assert_eq!(store.git_commit_locators().unwrap().len(), 2);
}

#[test]
fn status_reports_last_index_run_timing() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("README.md"), "# Notes\n\nBody.\n").unwrap();

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let stats = store.stats().unwrap();
    let run = stats
        .last_index_run
        .clone()
        .expect("status exposes the last index run");
    assert!(run.finished_at > 0);
    assert!(run.duration_ms >= 0);
    assert!(run.changed_sources >= 1);

    let rendered = serde_json::to_string(&stats).unwrap();
    assert!(
        rendered.contains("duration_ms"),
        "serialized status: {rendered}"
    );
}

const SESSION_HEAD: &[&str] = &[
    r#"{"type":"session","version":3,"id":"inc-0001","timestamp":"2026-08-20T10:00:00.000Z","cwd":"/tmp/snoop-incremental"}"#,
    r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2026-08-20T10:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Investigate the refresh_session loop"}]}}"#,
    r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-08-20T10:01:05.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Found stale token reuse."}]}}"#,
];

const SESSION_APPEND: &[&str] = &[
    r#"{"type":"message","id":"u2","parentId":"a1","timestamp":"2026-08-20T10:05:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Now run the auth tests"}]}}"#,
    r#"{"type":"message","id":"a2","parentId":"u2","timestamp":"2026-08-20T10:05:02.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"c9","name":"bash","arguments":{"command":"cargo test auth"}}]}}"#,
];

#[test]
fn session_append_reembeds_only_new_episodes() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("README.md"), "# Fixture\n").unwrap();
    let canonical = directory.path().canonicalize().unwrap();
    let session_directory =
        sessions_root
            .path()
            .join(snoop::ingest::harness::session_directory_name(
                &canonical.to_string_lossy(),
            ));
    std::fs::create_dir_all(&session_directory).unwrap();
    let session_path = session_directory.join("2026-08-20T10-00-00-000Z_inc-0001.jsonl");
    std::fs::write(&session_path, SESSION_HEAD.join("\n") + "\n").unwrap();
    std::env::set_var("SNOOP_SESSIONS_ROOT", sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let first =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(count_kind(&store, SourceKind::AgentSession), 1);
    assert!(first.embedded > 0);

    let mut appended = SESSION_HEAD.to_vec();
    appended.extend_from_slice(SESSION_APPEND);
    std::fs::write(&session_path, appended.join("\n") + "\n").unwrap();

    let second =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(
        count_kind(&store, SourceKind::AgentSession),
        2,
        "appended user turn becomes a new episode"
    );
    assert!(
        second.units_reused >= 1,
        "unchanged episode units must be reused: {:?}",
        second
    );
    assert_eq!(
        second.embedded, 2,
        "only the new episode's evidence and routing vectors are embedded: {:?}",
        second
    );

    std::env::remove_var("SNOOP_SESSIONS_ROOT");
}

#[test]
fn timed_out_rebuild_does_not_mark_format_current() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src")).unwrap();
    std::fs::write(
        directory.path().join("src/auth.rs"),
        "pub fn refresh_session() {}\n",
    )
    .unwrap();
    git(directory.path(), &["init", "--quiet"]);
    git(directory.path(), &["add", "."]);
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "first: introduce refresh"],
    );

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let first =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    store
        .set_repository_content_version("stale-version")
        .unwrap();
    let expired = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let outcome =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), Some(expired))
            .unwrap();
    assert!(outcome.timed_out);

    let repository = store.repository().unwrap().expect("repository exists");
    assert_eq!(repository.content_version, "stale-version");

    let retry =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert!(!retry.timed_out);
    let repository = store.repository().unwrap().expect("repository exists");
    assert_eq!(
        repository.content_version,
        snoop::ingest::INDEX_FORMAT_VERSION
    );
}
