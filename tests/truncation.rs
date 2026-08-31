//! Defect-audit 20260831023057-8ecdc8ca c6: anchor-lookup truncation must be
//! visible, not silent. MCP marks the result object with `truncated: true`;
//! the CLI prints a "+N more units not shown" notice on stderr.

use std::process::Command;

use snoop::core::{hash_segments, AnchorKind, BuiltAnchor, BuiltUnit, SourceKind, UnitKind};
use snoop::mcp::handle_message;
use snoop::store::{SourceIngest, Store};

fn anchored_units(symbol: &str, count: usize) -> Vec<BuiltUnit> {
    (0..count)
        .map(|i| {
            let evidence = format!("unit {i} discusses {symbol}");
            BuiltUnit {
                kind: UnitKind::Prose,
                evidence_text: evidence.clone(),
                routing_text: evidence.clone(),
                token_count: 3,
                content_hash: hash_segments(&[&evidence]),
                metadata: serde_json::json!({}),
                anchors: vec![BuiltAnchor {
                    kind: AnchorKind::Symbol,
                    value: symbol.to_string(),
                    relationship: "mentions".to_string(),
                }],
            }
        })
        .collect()
}

fn commit_units(store: &mut Store, locator: &str, units: &[BuiltUnit]) {
    store
        .commit_source(SourceIngest {
            kind: SourceKind::Markdown,
            locator,
            content_hash: "source-hash",
            modified_at: None,
            metadata: serde_json::json!({}),
            units,
        })
        .unwrap();
}

#[test]
fn mcp_symbol_context_reports_truncation_beyond_the_display_cap() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    commit_units(&mut store, "/repo/big.md", &anchored_units("big", 80));
    commit_units(&mut store, "/repo/small.md", &anchored_units("small", 1));

    let call = |symbol: &str, id: i64| {
        handle_message(
            &store,
            None,
            &serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":"repo_symbol_context","arguments":{"symbol":symbol}}}),
        )
        .unwrap()
    };

    // Over the cap: oldest page only, with the additive `truncated` flag.
    let response = call("big", 1);
    assert_eq!(
        response["result"]["truncated"], true,
        "the 80-unit lookup must be marked truncated: {response}"
    );
    let entries: serde_json::Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let list = entries.as_array().expect("bare array payload");
    assert_eq!(list.len(), 64, "the page is capped at ANCHOR_LOOKUP_LIMIT");

    // Under the cap: no truncated field at all (additive, not always-on).
    let response = call("small", 2);
    assert!(
        response["result"].get("truncated").is_none(),
        "a page within the cap must not carry the flag: {response}"
    );
    let entries: serde_json::Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(entries.as_array().unwrap().len(), 1);
}

#[test]
fn cli_history_prints_a_truncation_notice() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("index.db");
    {
        let mut store = Store::open(&db).unwrap();
        let root = directory.path().canonicalize().unwrap();
        store.bind_repository(&root.to_string_lossy()).unwrap();
        commit_units(&mut store, "/repo/big.md", &anchored_units("big", 80));
    }

    let output = Command::new(env!("CARGO_BIN_EXE_snoop"))
        .args(["history", "big", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(stdout.is_array(), "history still prints a JSON array");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("more units not shown"),
        "the notice must say the answer is a prefix: {stderr}"
    );
    assert!(
        stderr.contains("16"),
        "the notice must count the hidden units: {stderr}"
    );
}

#[test]
fn units_for_anchor_counts_units_beyond_the_page() {
    // Store-level boundary: oldest page plus a hidden-unit count.
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    commit_units(&mut store, "/repo/n.md", &anchored_units("big", 5));

    let (ids, more) = store.units_for_anchor("symbol", "big", 3).unwrap();
    assert_eq!(ids.len(), 3);
    assert_eq!(more, 2);

    let (ids, more) = store.units_for_anchor("symbol", "big", 5).unwrap();
    assert_eq!(ids.len(), 5);
    assert_eq!(more, 0, "exact fit is not truncated");

    let (ids, more) = store.units_for_anchor("symbol", "big", 10).unwrap();
    assert_eq!(ids.len(), 5);
    assert_eq!(more, 0, "under capacity is not truncated");

    assert_eq!(
        store.units_for_anchor("symbol", "missing", 4).unwrap(),
        (Vec::new(), 0)
    );
}
