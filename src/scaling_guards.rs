//! Structural scaling guards from the computational-scaling-audit
//! (run 20260830195149-6f1a96a5). These assert the repaired sites keep their
//! no-scan shape; op-count invariants live next to the implementations they
//! bound (align, code, git history, store, harness tests).

use std::path::Path;

fn normalized(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(relative);
    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {relative}: {error}"));
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn repaired_sites_keep_no_scan_shape() {
    // Finding 1: consumed_old is a bool flag array, not a scanned Vec.
    assert!(
        !normalized("ingest/git/align.rs").contains("consumed_old.contains"),
        "align must not scan a consumed Vec (finding 1)"
    );
    // Finding 2: boundary lines come from the monotone cursor, not from-0 rescans.
    assert!(
        !normalized("ingest/code.rs").contains("source.as_bytes()[..capped]"),
        "code boundaries must not rescan from byte 0 per boundary (finding 2)"
    );
    // Finding 3: parent resolution hoisted to one call per commit; blob reads
    // stream through cat-file, not per-file `git show` spawns.
    assert_eq!(
        normalized("ingest/git/emit.rs")
            .matches("parent_oid(root, &commit.oid)")
            .count(),
        1,
        "parent_oid must be called exactly once per commit (finding 3)"
    );
    assert!(
        !normalized("ingest/git/history.rs").contains("\"show\", &rev_path"),
        "blob reads must go through the cat-file session (finding 3)"
    );
    // Finding 4: reused rows relocate through the id map.
    assert!(
        !normalized("store.rs").contains("old_units .iter() .find"),
        "reused rows must not be relocated by linear scan (finding 4)"
    );
    // Finding 5: results attach via the call-event index, not a backward scan.
    assert!(
        !normalized("ingest/harness/jsonl.rs").contains(".iter_mut().rev().find("),
        "tool results must attach via the call-event index (finding 5)"
    );
}
