use std::path::Path;
use std::sync::{Mutex, OnceLock};

use snoop::core::SourceKind;
use snoop::ingest::index_repository_bounded;
use snoop::store::Store;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn set_muse_root(root: &Path) {
    std::env::set_var("SNOOP_MUSE_ROOT", root);
}

fn clear_muse_env() {
    std::env::remove_var("SNOOP_MUSE_ROOT");
    std::env::remove_var("SNOOP_SESSIONS_ROOT");
}

const SESSION_ID: &str = "muse-test-session-0001";
const BASE_US: i64 = 1_700_000_000_000_000;

fn repo_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session() {\n    validate();\n}\n\nfn validate() {}\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "# Fixture\n\nAuth module.\n").unwrap();
}

fn env_line(sequence: u64, payload_type: &str, payload: serde_json::Value) -> String {
    serde_json::json!({
        "schema_version": 1,
        "id": format!("rec-{sequence}"),
        "stream": {"kind": "session", "id": SESSION_ID},
        "sequence": sequence,
        "recorded_at": BASE_US + sequence as i64 * 1_000,
        "record_type": "event",
        "durability": "durable",
        "causation_id": null,
        "payload_type": payload_type,
        "payload_schema_version": 1,
        "payload": payload,
    })
    .to_string()
}

fn run_event(run_id: &str, event: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"kind": "run", "run_id": run_id, "event": event})
}

fn completed_run_lines(run_id: &str, start: u64, prompt: &str, answer: &str) -> Vec<String> {
    vec![
        env_line(
            start,
            "runtime.session",
            run_event(
                run_id,
                serde_json::json!({"kind": "started", "prompt": prompt}),
            ),
        ),
        env_line(
            start + 1,
            "runtime.session",
            run_event(
                run_id,
                serde_json::json!({"kind": "assistant_message_committed", "text": answer}),
            ),
        ),
        env_line(
            start + 2,
            "runtime.session",
            run_event(
                run_id,
                serde_json::json!({"kind": "terminal", "terminal": "completed"}),
            ),
        ),
    ]
}

fn session_end_line(sequence: u64) -> String {
    env_line(
        sequence,
        "session.end",
        serde_json::json!({
            "kind": "session_end",
            "record": {"session_id": SESSION_ID, "exit_reason": "complete"},
        }),
    )
}

/// Minimal index schema: exactly the columns the adapter selects.
fn create_index(db_path: &Path) {
    let connection = rusqlite::Connection::open(db_path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sessions (
               session_id TEXT PRIMARY KEY,
               session_log_path TEXT NOT NULL,
               layout TEXT NOT NULL,
               workspace_root TEXT,
               status TEXT NOT NULL,
               session_name TEXT,
               model_id TEXT,
               created_at_us INTEGER,
               updated_at_us INTEGER,
               source_fingerprint TEXT
             )",
        )
        .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn insert_row(
    db_path: &Path,
    session_id: &str,
    log_path: &str,
    layout: &str,
    workspace_root: Option<&str>,
    status: &str,
    session_name: Option<&str>,
) {
    let connection = rusqlite::Connection::open(db_path).unwrap();
    connection
        .execute(
            "INSERT INTO sessions(session_id, session_log_path, layout, workspace_root, status, \
             session_name, model_id, created_at_us, updated_at_us, source_fingerprint) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                session_id,
                log_path,
                layout,
                workspace_root,
                status,
                session_name,
                Some("muse-spark-test"),
                Some(BASE_US),
                Some(BASE_US + 1_000_000),
                Some("fp-1"),
            ],
        )
        .unwrap();
}

fn write_log(muse_root: &Path, relative: &str, lines: &[String]) {
    let path = muse_root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn muse_units(store: &Store) -> Vec<snoop::core::RetrievalUnit> {
    store
        .unit_ids()
        .unwrap()
        .into_iter()
        .filter_map(|id| store.unit_by_id(id).unwrap())
        .filter(|unit| unit.locator.starts_with("muse-session:"))
        .collect()
}

/// One ended session for this repo plus one foreign-workspace row that
/// must never cross-index.
fn two_session_tree(repo: &Path, muse_root: &Path) {
    let canonical = repo.canonicalize().unwrap();
    create_index(&muse_root.join("session-index.db"));
    let mut lines =
        completed_run_lines("run-1", 1, "Rotate the stale token", "Rotated in refresh.");
    lines.push(session_end_line(4));
    write_log(muse_root, "sessions/ended/session.jsonl", &lines);
    insert_row(
        &muse_root.join("session-index.db"),
        SESSION_ID,
        &muse_root
            .join("sessions/ended/session.jsonl")
            .to_string_lossy(),
        "session_jsonl",
        Some(&canonical.to_string_lossy()),
        "valid",
        Some("test-name"),
    );
    let mut foreign = completed_run_lines("run-foreign", 1, "Foreign prompt", "Foreign answer.");
    foreign.push(session_end_line(4));
    write_log(muse_root, "sessions/foreign/session.jsonl", &foreign);
    insert_row(
        &muse_root.join("session-index.db"),
        "muse-test-foreign-0002",
        &muse_root
            .join("sessions/foreign/session.jsonl")
            .to_string_lossy(),
        "session_jsonl",
        Some("/somewhere/else"),
        "valid",
        Some("foreign-name"),
    );
}

#[test]
fn ended_session_ingests_as_episode_with_references_not_evidence() {
    let _guard = env_lock();
    let repo = tempfile::tempdir().unwrap();
    let muse_root = tempfile::tempdir().unwrap();
    repo_fixture(repo.path());
    two_session_tree(repo.path(), muse_root.path());
    set_muse_root(muse_root.path());

    let before: Vec<_> = std::fs::read_dir(muse_root.path()).unwrap().collect();

    let mut store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut store, repo.path(), None, None).unwrap();

    let units = muse_units(&store);
    assert_eq!(
        units.len(),
        1,
        "only the ended own-workspace session indexes: {:?}",
        units.iter().map(|unit| &unit.locator).collect::<Vec<_>>()
    );
    let unit = &units[0];
    assert_eq!(unit.locator, format!("muse-session:{SESSION_ID}"));
    assert_eq!(unit.source_kind, SourceKind::AgentSession);
    assert!(unit.evidence_text.contains("Rotate the stale token"));
    assert!(unit.evidence_text.contains("Rotated in refresh."));
    assert!(unit.routing_text.contains("source: agent_episode"));
    assert_eq!(unit.metadata["episode"], 1);
    assert_eq!(unit.metadata["run_id"], "run-1");
    assert_eq!(unit.metadata["harness"], "muse");
    assert!(unit.timestamp.is_some());

    let (anchored, more) = store.units_for_anchor("session", SESSION_ID, 32).unwrap();
    assert_eq!(more, 0);
    assert_eq!(anchored.len(), 1, "session anchor joins to the episode");

    // Read-only index access: the adapter must not leave write artifacts.
    let after: Vec<_> = std::fs::read_dir(muse_root.path()).unwrap().collect();
    let names = |entries: &[std::io::Result<std::fs::DirEntry>]| {
        entries
            .iter()
            .filter_map(|entry| entry.as_ref().ok())
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&before), names(&after));

    clear_muse_env();
}

#[test]
fn active_session_without_end_marker_is_excluded() {
    let _guard = env_lock();
    let repo = tempfile::tempdir().unwrap();
    let muse_root = tempfile::tempdir().unwrap();
    repo_fixture(repo.path());
    let canonical = repo.path().canonicalize().unwrap();
    create_index(&muse_root.path().join("session-index.db"));
    // Completed run but no durable session.end: still live context.
    let lines = completed_run_lines("run-live", 1, "Live prompt stays out", "Live answer.");
    write_log(muse_root.path(), "sessions/live/session.jsonl", &lines);
    insert_row(
        &muse_root.path().join("session-index.db"),
        SESSION_ID,
        &muse_root
            .path()
            .join("sessions/live/session.jsonl")
            .to_string_lossy(),
        "session_jsonl",
        Some(&canonical.to_string_lossy()),
        "valid",
        Some("live-name"),
    );
    set_muse_root(muse_root.path());

    let mut store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    assert!(
        muse_units(&store).is_empty(),
        "live sessions contribute no units"
    );

    clear_muse_env();
}

#[test]
fn missing_optional_metadata_and_odd_rows_still_index_or_skip_safely() {
    let _guard = env_lock();
    let repo = tempfile::tempdir().unwrap();
    let muse_root = tempfile::tempdir().unwrap();
    repo_fixture(repo.path());
    let canonical = repo.path().canonicalize().unwrap();
    create_index(&muse_root.path().join("session-index.db"));
    let mut lines = completed_run_lines("run-1", 1, "Sparse metadata run", "Sparse answer.");
    lines.push(session_end_line(4));
    write_log(muse_root.path(), "sessions/sparse/session.jsonl", &lines);
    // NULL session_name/model_id/timestamps must not block ingestion.
    let connection = rusqlite::Connection::open(muse_root.path().join("session-index.db")).unwrap();
    connection
        .execute(
            "INSERT INTO sessions(session_id, session_log_path, layout, workspace_root, status) \
             VALUES (?1, ?2, 'session_jsonl', ?3, 'valid')",
            rusqlite::params![
                SESSION_ID,
                muse_root
                    .path()
                    .join("sessions/sparse/session.jsonl")
                    .to_string_lossy(),
                canonical.to_string_lossy(),
            ],
        )
        .unwrap();
    // Unsupported layout and status rows skip loudly, never aborting.
    insert_row(
        &muse_root.path().join("session-index.db"),
        "muse-test-oddball-layout",
        muse_root
            .path()
            .join("sessions/sparse/session.jsonl")
            .to_string_lossy()
            .as_ref(),
        "future_layout",
        Some(&canonical.to_string_lossy()),
        "valid",
        None,
    );
    insert_row(
        &muse_root.path().join("session-index.db"),
        "muse-test-oddball-status",
        muse_root
            .path()
            .join("sessions/sparse/session.jsonl")
            .to_string_lossy()
            .as_ref(),
        "session_jsonl",
        Some(&canonical.to_string_lossy()),
        "archived",
        None,
    );
    set_muse_root(muse_root.path());

    let mut store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    let units = muse_units(&store);
    assert_eq!(units.len(), 1);
    assert!(units[0].metadata["session_name"].is_null());

    clear_muse_env();
}

#[test]
fn missing_index_is_a_successful_empty_discovery() {
    let _guard = env_lock();
    let repo = tempfile::tempdir().unwrap();
    let muse_root = tempfile::tempdir().unwrap();
    repo_fixture(repo.path());
    // No session-index.db at all: Muse never ran here.
    set_muse_root(&muse_root.path().join("does-not-exist"));

    let mut store = Store::open_in_memory().unwrap();
    let outcome = index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    assert!(muse_units(&store).is_empty());
    assert!(
        outcome.changed_sources > 0,
        "the repository's own sources still index"
    );

    clear_muse_env();
}

#[test]
fn failed_discovery_retains_prior_muse_history() {
    let _guard = env_lock();
    let repo = tempfile::tempdir().unwrap();
    let muse_root = tempfile::tempdir().unwrap();
    repo_fixture(repo.path());
    two_session_tree(repo.path(), muse_root.path());
    set_muse_root(muse_root.path());

    let mut store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    assert_eq!(muse_units(&store).len(), 1);

    // Corrupt the index: discovery now fails transiently.
    std::fs::write(muse_root.path().join("session-index.db"), b"not a database").unwrap();
    let outcome = index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    assert_eq!(outcome.deleted_sources, 0);
    assert_eq!(
        muse_units(&store).len(),
        1,
        "a failed Muse discovery must not purge established history"
    );

    clear_muse_env();
}

#[test]
fn successful_empty_discovery_purges_stale_muse_history() {
    let _guard = env_lock();
    let repo = tempfile::tempdir().unwrap();
    let muse_root = tempfile::tempdir().unwrap();
    repo_fixture(repo.path());
    two_session_tree(repo.path(), muse_root.path());
    set_muse_root(muse_root.path());

    let mut store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    assert_eq!(muse_units(&store).len(), 1);

    // The index is gone but readable-as-absent: an authoritative empty set.
    std::fs::remove_file(muse_root.path().join("session-index.db")).unwrap();
    let outcome = index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    assert!(
        muse_units(&store).is_empty(),
        "authoritative empty discovery purges stale Muse sources"
    );
    assert!(outcome.deleted_sources >= 1);

    clear_muse_env();
}

#[test]
fn pi_and_muse_sessions_coexist_in_distinct_namespaces() {
    let _guard = env_lock();
    let repo = tempfile::tempdir().unwrap();
    let muse_root = tempfile::tempdir().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    repo_fixture(repo.path());
    two_session_tree(repo.path(), muse_root.path());
    set_muse_root(muse_root.path());

    let canonical = repo.path().canonicalize().unwrap();
    let directory = sessions_root
        .path()
        .join(snoop::ingest::harness::session_directory_name(
            &canonical.to_string_lossy(),
        ));
    std::fs::create_dir_all(&directory).unwrap();
    let header = format!(
        r#"{{"type":"session","version":3,"id":"pi-coexist-1","timestamp":"2026-08-20T10:00:00.000Z","cwd":"{}"}}"#,
        canonical.display()
    );
    let turn = r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2026-08-20T10:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Pi notes about rotation"}]}}"#;
    let compaction = r#"{"type":"compaction","id":"k1","parentId":"u1"}"#;
    std::fs::write(
        directory.join("pi.jsonl"),
        format!("{header}\n{turn}\n{compaction}\n"),
    )
    .unwrap();
    std::env::set_var("SNOOP_SESSIONS_ROOT", sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    let locators: Vec<String> = store
        .unit_ids()
        .unwrap()
        .into_iter()
        .filter_map(|id| store.unit_by_id(id).unwrap())
        .map(|unit| unit.locator)
        .filter(|locator| locator.contains("session:"))
        .collect();
    assert!(
        locators
            .iter()
            .any(|locator| locator.starts_with("pi-session:")),
        "pi sessions still index: {locators:?}"
    );
    assert!(
        locators
            .iter()
            .any(|locator| locator.starts_with("muse-session:")),
        "muse sessions index alongside: {locators:?}"
    );

    clear_muse_env();
}

#[test]
fn reindexing_unchanged_muse_sessions_is_a_noop() {
    let _guard = env_lock();
    let repo = tempfile::tempdir().unwrap();
    let muse_root = tempfile::tempdir().unwrap();
    repo_fixture(repo.path());
    two_session_tree(repo.path(), muse_root.path());
    set_muse_root(muse_root.path());

    let mut store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    let second = index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    assert_eq!(second.changed_sources, 0);
    assert_eq!(second.units_added, 0);
    assert_eq!(muse_units(&store).len(), 1);

    clear_muse_env();
}

#[test]
fn appending_a_sealed_run_reuses_prior_unit_rows() {
    let _guard = env_lock();
    let repo = tempfile::tempdir().unwrap();
    let muse_root = tempfile::tempdir().unwrap();
    repo_fixture(repo.path());
    let canonical = repo.path().canonicalize().unwrap();
    create_index(&muse_root.path().join("session-index.db"));
    let log_relative = "sessions/growing/session.jsonl";
    let mut lines = completed_run_lines("run-1", 1, "First prompt", "First answer.");
    lines.push(session_end_line(4));
    write_log(muse_root.path(), log_relative, &lines);
    insert_row(
        &muse_root.path().join("session-index.db"),
        SESSION_ID,
        &muse_root.path().join(log_relative).to_string_lossy(),
        "session_jsonl",
        Some(&canonical.to_string_lossy()),
        "valid",
        Some("growing-name"),
    );
    set_muse_root(muse_root.path());

    let mut store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    let before: Vec<String> = muse_units(&store)
        .iter()
        .map(|unit| unit.content_hash.clone())
        .collect();
    assert_eq!(before.len(), 1);

    // A later sealed run appends after a new end marker.
    let mut grown = completed_run_lines("run-1", 1, "First prompt", "First answer.");
    grown.extend(completed_run_lines(
        "run-2",
        10,
        "Second prompt",
        "Second answer.",
    ));
    grown.push(session_end_line(13));
    write_log(muse_root.path(), log_relative, &grown);
    let second = index_repository_bounded(&mut store, repo.path(), None, None).unwrap();
    let after: Vec<String> = muse_units(&store)
        .iter()
        .map(|unit| unit.content_hash.clone())
        .collect();
    assert_eq!(after.len(), 2);
    assert!(
        after.contains(&before[0]),
        "the first run keeps its stable identity"
    );
    assert!(
        second.units_reused >= 1,
        "unchanged units reuse rows and embeddings: {:?}",
        second
    );

    clear_muse_env();
}
