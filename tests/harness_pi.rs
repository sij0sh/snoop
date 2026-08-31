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
const SESSION_LINES: &[&str] = &[
    r#"{"type":"session","version":3,"id":"019f-test-snoop-0001","timestamp":"2026-08-20T10:00:00.000Z","cwd":"/tmp/snoop-harness-fixture"}"#,
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
    std::fs::write(file, SESSION_LINES.join("\n") + "\n").unwrap();
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
