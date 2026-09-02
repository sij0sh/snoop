//! Anchor-lookup truncation must be visible. The CLI prints a
//! "+N more units not shown" notice on stderr.

use std::process::Command;

use snoop::core::{hash_segments, AnchorKind, BuiltAnchor, BuiltUnit, SourceKind, UnitKind};
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
fn cli_inspect_symbol_prints_a_truncation_notice() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("index.db");
    {
        let mut store = Store::open(&db).unwrap();
        let root = directory.path().canonicalize().unwrap();
        store.bind_repository(&root.to_string_lossy()).unwrap();
        commit_units(&mut store, "/repo/big.md", &anchored_units("big", 80));
    }

    let output = Command::new(env!("CARGO_BIN_EXE_snoop"))
        .args(["inspect", "symbol", "big", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        stdout.is_array(),
        "inspect symbol still prints a JSON array"
    );
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
