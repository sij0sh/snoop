use std::path::Path;
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

const SESSION_ID: &str = "019f-test-snoop-0001";
// The session header carries no cwd; fixture() prepends one built from the
// canonical repo root, which is the discovery attribution key.
const SESSION_LINES: &[&str] = &[
    r#"{"type":"model_change","id":"m1","parentId":null,"timestamp":"2026-08-20T10:00:00.100Z"}"#,
    r#"{"type":"message","id":"u1","parentId":"m1","timestamp":"2026-08-20T10:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Investigate why refresh_session loops forever"}]}}"#,
    r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-08-20T10:01:05.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"private reasoning"},{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"src/auth.rs"}}]}}"#,
    r#"{"type":"message","id":"r1","parentId":"a1","timestamp":"2026-08-20T10:01:05.100Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"read","content":[{"type":"text","text":"line 1\nline 2\nline 3\n"}]}}"#,
    r#"{"type":"message","id":"a2","parentId":"r1","timestamp":"2026-08-20T10:02:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"The loop comes from stale token reuse in refresh_session."},{"type":"toolCall","id":"c2","name":"edit","arguments":{"path":"src/auth.rs","oldText":"fn refresh() {}","newText":"fn refresh_with_rotation() {}"}}]}}"#,
    r#"{"type":"message","id":"u2","parentId":"a2","timestamp":"2026-08-20T10:05:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Now run the auth tests"}]}}"#,
    r#"{"type":"message","id":"a3","parentId":"u2","timestamp":"2026-08-20T10:05:02.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"c3","name":"bash","arguments":{"command":"cargo test auth"}}]}}"#,
    r#"{"type":"compaction","id":"k1","parentId":"a3","timestamp":"2026-08-20T10:06:00.000Z"}"#,
    r#"{"type":"custom","id":"x9","parentId":"k1","timestamp":"2026-08-20T10:06:01.000Z","payload":{"unknown":true}}"#,
    r#"{"type":"totally_unknown_future_type","id":"z1","parentId":"x9"}"#,
];

fn fixture(root: &Path, sessions_root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session() {\n    validate();\n}\n\nfn validate() {}\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "# Fixture\n\nAuth module.\n").unwrap();

    let canonical = root.canonicalize().unwrap();
    let directory = sessions_root.join(snoop::ingest::harness::session_directory_name(
        &canonical.to_string_lossy(),
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("2026-08-20T10-00-00-000Z_019f-test-snoop-0001.jsonl");
    let header = format!(
        r#"{{"type":"session","version":3,"id":"{SESSION_ID}","timestamp":"2026-08-20T10:00:00.000Z","cwd":"{}"}}"#,
        canonical.display()
    );
    let mut lines = vec![header];
    lines.extend(SESSION_LINES.iter().map(|line| line.to_string()));
    std::fs::write(file, lines.join("\n") + "\n").unwrap();
}

fn env_with_sessions_root(root: &Path) {
    std::env::set_var("SNOOP_SESSIONS_ROOT", root);
}
#[test]
fn sessions_index_as_episodes_with_references_not_evidence() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    fixture(directory.path(), sessions_root.path());
    env_with_sessions_root(sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let episode_units: Vec<_> = store
        .unit_ids()
        .unwrap()
        .into_iter()
        .filter_map(|id| store.unit_by_id(id).unwrap())
        .filter(|unit| unit.source_kind == SourceKind::AgentSession)
        .collect();
    assert_eq!(
        episode_units.len(),
        2,
        "unit count tracks user-turn count: {:?}",
        episode_units
            .iter()
            .map(|unit| &unit.locator)
            .collect::<Vec<_>>()
    );

    for unit in &episode_units {
        assert!(unit.locator.starts_with("pi-session:"));
        assert!(unit.evidence_text.contains("User:"));
        assert!(unit.routing_text.contains("source: agent_episode"));
        let episode_number = unit.metadata["episode"].as_u64().unwrap();
        assert!(episode_number >= 1);
    }

    let first = &episode_units
        .iter()
        .find(|unit| unit.metadata["episode"] == 1)
        .unwrap();
    assert!(first
        .evidence_text
        .contains("Investigate why refresh_session loops"));
    assert!(first
        .evidence_text
        .contains("The loop comes from stale token reuse"));
    assert!(first.evidence_text.contains("read src/auth.rs"));
    assert!(
        !first.evidence_text.contains("line 1\nline 2"),
        "tool output bodies must not become evidence"
    );
    assert!(first.metadata["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "src/auth.rs"));

    let second = &episode_units
        .iter()
        .find(|unit| unit.metadata["episode"] == 2)
        .unwrap();
    assert!(second.metadata["commands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "cargo test auth"));
    assert!(second.timestamp.is_some());

    std::env::remove_var("SNOOP_SESSIONS_ROOT");
}

#[test]
fn sessions_table_populated_and_query_returns_agent_evidence() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    fixture(directory.path(), sessions_root.path());
    env_with_sessions_root(sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let (session_units, session_more) = store.units_for_anchor("session", SESSION_ID, 32).unwrap();
    assert_eq!(session_more, 0);
    assert_eq!(
        session_units.len(),
        2,
        "session anchor joins to both episodes"
    );

    let report = query(
        &store,
        Some(&embedder),
        "refresh_session loop stale token",
        &QueryOptions {
            channels: QueryChannels::for_embedder(Some(&embedder)),
            top_n: 10,
            max_tokens: 4_000,
            diagnostics: true,
            ..QueryOptions::default()
        },
    )
    .unwrap();
    let diagnostics = report
        .debug
        .as_ref()
        .expect("session anchors come from diagnostics");
    let agent: Vec<_> = report
        .packet
        .items
        .iter()
        .zip(diagnostics.items.iter())
        .filter(|(item, _)| item.source_kind == SourceKind::AgentSession)
        .collect();
    assert!(
        !agent.is_empty(),
        "query returns prior-agent evidence, kinds: {:?}",
        report
            .packet
            .items
            .iter()
            .map(|item| item.source_kind)
            .collect::<Vec<_>>()
    );
    let (_, agent_item) = agent[0];
    assert!(agent_item
        .anchors
        .iter()
        .any(|anchor| anchor.kind == snoop::core::AnchorKind::Session));
    assert!(agent_item
        .anchors
        .iter()
        .any(|anchor| anchor.value.contains("src/auth.rs")));

    std::env::remove_var("SNOOP_SESSIONS_ROOT");
}

#[test]
fn reindexing_unchanged_sessions_is_a_noop() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    fixture(directory.path(), sessions_root.path());
    env_with_sessions_root(sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    let before = store.unit_ids().unwrap();
    let second =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(second.changed_sources, 0);
    assert_eq!(second.units_added, 0);

    std::env::remove_var("SNOOP_SESSIONS_ROOT");
    let _ = before;
}

fn git(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .expect("git spawns");
    assert!(status.success(), "git {args:?} failed");
}

fn write_session_file(directory: &Path, name: &str, session_id: &str, cwd: Option<&str>, token: &str) {
    let header = match cwd {
        Some(cwd) => format!(
            r#"{{"type":"session","version":3,"id":"{session_id}","timestamp":"2026-08-20T10:00:00.000Z","cwd":"{cwd}"}}"#
        ),
        None => format!(
            r#"{{"type":"session","version":3,"id":"{session_id}","timestamp":"2026-08-20T10:00:00.000Z"}}"#
        ),
    };
    let turn = format!(
        r#"{{"type":"message","id":"u1","parentId":null,"timestamp":"2026-08-20T10:01:00.000Z","message":{{"role":"user","content":[{{"type":"text","text":"Notes about {token}"}}]}}}}"#
    );
    std::fs::write(directory.join(name), format!("{header}\n{turn}\n")).unwrap();
}

fn session_locators(store: &Store) -> Vec<String> {
    store
        .unit_ids()
        .unwrap()
        .into_iter()
        .filter_map(|id| store.unit_by_id(id).unwrap())
        .map(|unit| unit.locator)
        .filter(|locator| locator.starts_with("pi-session:"))
        .collect()
}

#[test]
fn colliding_session_directory_names_stay_attributed_by_cwd() {
    let _guard = env_lock();
    let base = tempfile::tempdir().unwrap();
    // The mangling rule turns '/' and '-' into '-', so these two roots
    // flatten to the same sessions directory.
    let dash = base.path().join("a-b").join("c");
    let slash = base.path().join("a").join("b").join("c");
    std::fs::create_dir_all(&dash).unwrap();
    std::fs::create_dir_all(&slash).unwrap();
    git(&dash, &["init", "--quiet"]);
    git(&slash, &["init", "--quiet"]);
    let dash = dash.canonicalize().unwrap();
    let slash = slash.canonicalize().unwrap();
    assert_eq!(
        snoop::ingest::harness::session_directory_name(&dash.to_string_lossy()),
        snoop::ingest::harness::session_directory_name(&slash.to_string_lossy()),
        "precondition: the two roots collide on one directory name"
    );

    let sessions_root = tempfile::tempdir().unwrap();
    let shared = sessions_root.path().join(
        snoop::ingest::harness::session_directory_name(&dash.to_string_lossy()),
    );
    std::fs::create_dir_all(&shared).unwrap();
    write_session_file(
        &shared,
        "s1.jsonl",
        "collide-dash-a1",
        Some(&dash.to_string_lossy()),
        "quartz-dash-token",
    );
    write_session_file(
        &shared,
        "s2.jsonl",
        "collide-slash-b2",
        Some(&slash.to_string_lossy()),
        "quartz-slash-token",
    );
    env_with_sessions_root(sessions_root.path());

    let mut dash_store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut dash_store, &dash, None, None).unwrap();
    assert_eq!(
        session_locators(&dash_store),
        vec!["pi-session:collide-dash-a1".to_string()],
        "the dash repo must index only its own session"
    );

    let mut slash_store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut slash_store, &slash, None, None).unwrap();
    assert_eq!(
        session_locators(&slash_store),
        vec!["pi-session:collide-slash-b2".to_string()],
        "the slash repo must index only its own session"
    );
}

#[test]
fn session_with_foreign_or_missing_cwd_is_skipped_as_foreign() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    let canonical = directory.path().canonicalize().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    let shared = sessions_root.path().join(
        snoop::ingest::harness::session_directory_name(&canonical.to_string_lossy()),
    );
    std::fs::create_dir_all(&shared).unwrap();
    write_session_file(
        &shared,
        "own.jsonl",
        "cwd-own-c1",
        Some(&canonical.to_string_lossy()),
        "own-session-token",
    );
    write_session_file(
        &shared,
        "foreign.jsonl",
        "cwd-foreign-f2",
        Some("/somewhere/else"),
        "foreign-session-token",
    );
    write_session_file(
        &shared,
        "legacy.jsonl",
        "cwd-missing-m3",
        None,
        "legacy-session-token",
    );
    env_with_sessions_root(sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut store, directory.path(), None, None).unwrap();
    assert_eq!(
        session_locators(&store),
        vec!["pi-session:cwd-own-c1".to_string()],
        "only the session whose header cwd is this repo root is indexed"
    );
}

#[test]
fn unreadable_sessions_directory_skips_instead_of_failing() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    fixture(directory.path(), sessions_root.path());
    env_with_sessions_root(sessions_root.path());

    let canonical = directory.path().canonicalize().unwrap();
    let mangled = sessions_root.path().join(
        snoop::ingest::harness::session_directory_name(&canonical.to_string_lossy()),
    );
    use std::os::unix::fs::PermissionsExt;
    let original = std::fs::metadata(&mangled).unwrap().permissions();
    std::fs::set_permissions(&mangled, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&mangled).is_ok() {
        // Root bypasses permission bits; the skip cannot be observed here.
        std::fs::set_permissions(&mangled, original).unwrap();
        return;
    }

    let mut store = Store::open_in_memory().unwrap();
    let outcome =
        index_repository_bounded(&mut store, directory.path(), None, None).unwrap();
    assert!(
        session_locators(&store).is_empty(),
        "an unreadable sessions directory serves no session data"
    );
    assert!(
        outcome.changed_sources > 0,
        "the run still indexes the repository's own sources"
    );

    std::fs::set_permissions(&mangled, original).unwrap();
    let _ = index_repository_bounded(&mut store, directory.path(), None, None).unwrap();
    assert!(
        session_locators(&store)
            .iter()
            .all(|locator| locator == "pi-session:019f-test-snoop-0001"),
        "restored permissions let the next run discover sessions again"
    );
}
