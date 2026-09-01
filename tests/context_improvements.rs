use std::process::Command;

use snoop::core::{hash_segments, AnchorKind, BuiltAnchor, BuiltUnit, SourceKind, UnitKind};
use snoop::store::{SourceIngest, Store};

fn unit(kind: UnitKind, text: &str, anchors: Vec<BuiltAnchor>, timestamp: i64) -> BuiltUnit {
    BuiltUnit {
        kind,
        evidence_text: text.to_string(),
        routing_text: text.to_string(),
        token_count: 4,
        content_hash: hash_segments(&[text]),
        metadata: serde_json::json!({"timestamp": timestamp}),
        anchors,
    }
}

fn fixture(db: &std::path::Path) {
    let mut store = Store::open(db).unwrap();
    store.bind_repository("/repo").unwrap();
    let file = BuiltAnchor {
        kind: AnchorKind::File,
        value: "src/auth.rs".into(),
        relationship: "touches".into(),
    };
    let symbol = BuiltAnchor {
        kind: AnchorKind::Symbol,
        value: "refresh_session".into(),
        relationship: "mentions".into(),
    };
    let session = BuiltAnchor {
        kind: AnchorKind::Session,
        value: "current-session".into(),
        relationship: "belongs_to".into(),
    };
    store
        .commit_source(SourceIngest {
            kind: SourceKind::AgentSession,
            locator: "pi-session:current-session",
            content_hash: "session-source",
            modified_at: Some(1_700_000_000),
            metadata: serde_json::json!({}),
            units: &[unit(
                UnitKind::Episode,
                "refresh_session alpha current prompt echo",
                vec![file.clone(), symbol.clone(), session],
                1_700_000_000,
            )],
        })
        .unwrap();
    store
        .commit_source(SourceIngest {
            kind: SourceKind::Code,
            locator: "src/auth.rs",
            content_hash: "code-source",
            modified_at: Some(1_700_000_000),
            metadata: serde_json::json!({}),
            units: &[unit(
                UnitKind::Code,
                "refresh_session alpha implementation",
                vec![file, symbol],
                1_700_000_000,
            )],
        })
        .unwrap();
}

#[test]
fn cli_excludes_sessions_from_query_and_symbol_inspection() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("index.db");
    fixture(&db);
    let binary = env!("CARGO_BIN_EXE_snoop");

    for args in [
        vec![
            "query",
            "refresh_session alpha",
            "--db",
            db.to_str().unwrap(),
            "--exclude-session",
            "current-session",
        ],
        vec![
            "inspect",
            "symbol",
            "refresh_session",
            "--db",
            db.to_str().unwrap(),
            "--exclude-session",
            "current-session",
        ],
    ] {
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
        let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let text = payload.to_string();
        assert!(!text.contains("pi-session:current-session"), "{text}");
        assert!(text.contains("src/auth.rs"), "{text}");
    }
}

#[test]
fn mcp_excludes_sessions_and_renders_timestamps() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("index.db");
    fixture(&db);
    let store = Store::open(&db).unwrap();
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "repo_symbol_context",
            "arguments": {
                "symbol": "refresh_session",
                "exclude_sessions": ["current-session"]
            }
        }
    });
    let response = snoop::mcp::handle_message(&store, None, &call).unwrap();
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(!text.contains("pi-session:current-session"), "{text}");
    assert!(text.contains("src/auth.rs"), "{text}");

    let query_call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "get_repo_context",
            "arguments": {
                "query": "refresh_session alpha",
                "exclude_sessions": ["current-session"]
            }
        }
    });
    let response = snoop::mcp::handle_message(&store, None, &query_call).unwrap();
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(!text.contains("pi-session:current-session"), "{text}");
    assert!(text.contains("src/auth.rs"), "{text}");

    let (entries, _) = snoop::mcp::symbol_context_entries(&store, "refresh_session").unwrap();
    let session = entries
        .iter()
        .find(|entry| entry["source_kind"] == "agent_session")
        .unwrap();
    assert!(session["timestamp"].is_string());
}

#[test]
fn query_and_inspect_emit_actionable_json_when_unindexed() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("empty.db");
    let binary = env!("CARGO_BIN_EXE_snoop");

    for args in [
        vec!["query", "anything", "--db", db.to_str().unwrap()],
        vec![
            "inspect",
            "symbol",
            "anything",
            "--db",
            db.to_str().unwrap(),
        ],
    ] {
        let output = Command::new(binary).args(args).output().unwrap();
        assert!(!output.status.success());
        let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["error"], "index a repository first");
        assert_eq!(payload["hint"], "run: snoop init .");
    }
}

#[test]
fn inspect_unit_renders_timestamp_as_text() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("index.db");
    fixture(&db);
    let store = Store::open(&db).unwrap();
    let id = store.units_for_source("src/auth.rs").unwrap()[0].id.0;
    drop(store);

    let output = Command::new(env!("CARGO_BIN_EXE_snoop"))
        .args([
            "inspect",
            "unit",
            &id.to_string(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(payload["unit"]["timestamp"].is_string());
}
