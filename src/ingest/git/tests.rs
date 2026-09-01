use super::align::{
    align_hunks, changed_new_lines, changed_old_lines, parse_hunks, reconcile_alignments,
    BoundaryConfidence, ChangeKind, Hunk, Side,
};
use super::emit::{push_units, split_hunk_text, PushContext};
use crate::core::{AtomKind, ParsedAtom};
use crate::ingest::units::{estimate_tokens, MAX_TOKENS};

#[test]
fn hunk_parser_reads_ranges_and_offsets() {
    let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,3 +1,4 @@ fn a\n+line\n@@ -10 +10,2 @@\n line\n";
    let hunks = parse_hunks(patch);
    assert_eq!(hunks.len(), 2);
    assert_eq!((hunks[0].new_start, hunks[0].new_count), (1, 4));
    assert_eq!(
        (
            hunks[1].old_start,
            hunks[1].old_count,
            hunks[1].new_start,
            hunks[1].new_count
        ),
        (10, 1, 10, 2)
    );
    assert!(hunks[0].text.starts_with("@@"));
    assert_eq!(
        hunks[0].end_offset,
        hunks[0].start_offset + hunks[0].text.len()
    );
}

#[test]
fn changed_new_lines_tracks_added_positions() {
    let text = "@@ -4,3 +4,4 @@\n context\n+added\n-removed\n+more\n".to_string();
    let hunk = Hunk {
        old_start: 4,
        old_count: 3,
        new_start: 4,
        new_count: 4,
        text,
        start_offset: 0,
        end_offset: 0,
    };
    assert_eq!(changed_new_lines(&hunk), vec![5, 6]);
}

#[test]
fn alignment_uses_changed_lines_and_falls_back_per_hunk() {
    let after = "fn validate() {}\n\nfn refresh_session() {\n    validate();\n}\n";
    let hunks = vec![Hunk {
        old_start: 3,
        old_count: 3,
        new_start: 3,
        new_count: 3,
        text: "@@ -3,3 +3,3 @@\n fn refresh_session() {\n-    nothing();\n+    validate();\n"
            .to_string(),
        start_offset: 0,
        end_offset: 0,
    }];
    let (groups, parsed) = align_hunks(&hunks, after, "src/auth.rs", Side::New);
    assert!(parsed);
    assert_eq!(groups.len(), 1);
    let (span, indices) = &groups[0];
    assert!(span
        .as_ref()
        .unwrap()
        .breadcrumb
        .contains("refresh_session"));
    assert_eq!(indices, &vec![0]);
}

#[test]
fn changed_old_lines_tracks_deleted_positions() {
    let text = "@@ -4,4 +4,3 @@\n context\n-removed\n+added\n-also_gone\n".to_string();
    let hunk = Hunk {
        old_start: 4,
        old_count: 4,
        new_start: 4,
        new_count: 3,
        text,
        start_offset: 0,
        end_offset: 0,
    };
    assert_eq!(changed_old_lines(&hunk), vec![5, 6]);
}

#[test]
fn whole_file_hunk_falls_back() {
    let after = "fn one() {}\nfn two() {}\n";
    let hunks = vec![Hunk {
        old_start: 0,
        old_count: 0,
        new_start: 1,
        new_count: 2,
        text: "@@\n".to_string(),
        start_offset: 0,
        end_offset: 3,
    }];
    let (groups, _parsed) = align_hunks(&hunks, after, "src/x.rs", Side::New);
    assert_eq!(groups.len(), 1);
    assert!(groups[0].0.is_none());
}

#[test]
fn reconcile_keeps_modified_symbol_identity() {
    let before = "fn kept() {\n    old_call();\n}\n\nfn other() {}\n";
    let after = "fn kept() {\n    new_call();\n}\n\nfn other() {}\n";
    let hunks = vec![Hunk {
        old_start: 1,
        old_count: 3,
        new_start: 1,
        new_count: 3,
        text: "@@ -1,3 +1,3 @@\n fn kept() {\n-    old_call();\n+    new_call();\n".to_string(),
        start_offset: 0,
        end_offset: 0,
    }];
    let alignments = reconcile_alignments(
        &hunks,
        before,
        after,
        "src/x.rs",
        "src/x.rs",
        false,
        ChangeKind::Modified,
    );
    let modified = alignments
        .iter()
        .find(|alignment| alignment.change_kind == ChangeKind::Modified)
        .expect("kept must be classified as modified");
    assert_eq!(modified.strategy, "symbol");
    let new_span = modified.new_span.as_ref().unwrap();
    assert_eq!(new_span.breadcrumb, "src/x.rs > kept");
    assert_eq!(modified.confidence, BoundaryConfidence::High);
    assert!(!alignments
        .iter()
        .any(|alignment| alignment.change_kind == ChangeKind::Added));
}

#[test]
fn reconcile_classifies_add_and_delete() {
    let before = "fn kept() {}\n\nfn gone() {\n    validate();\n}\n";
    let after = "fn kept() {}\n\nfn added() {\n    load();\n}\n";
    let hunks = vec![Hunk {
        old_start: 3,
        old_count: 3,
        new_start: 3,
        new_count: 3,
        text: "@@ -3,3 +3,3 @@\n fn kept() {}\n-fn gone() {\n-    validate();\n-}\n+fn added() {\n+    load();\n+}\n"
            .to_string(),
        start_offset: 0,
        end_offset: 0,
    }];
    let alignments = reconcile_alignments(
        &hunks,
        before,
        after,
        "src/x.rs",
        "src/x.rs",
        false,
        ChangeKind::Modified,
    );
    assert!(
        alignments.iter().any(|alignment| {
            alignment.change_kind == ChangeKind::Added
                && alignment
                    .new_span
                    .as_ref()
                    .is_some_and(|span| span.breadcrumb.ends_with("added"))
        }),
        "added must be classified: {alignments:?}"
    );
    assert!(
        alignments.iter().any(|alignment| {
            alignment.change_kind == ChangeKind::Deleted
                && alignment
                    .old_span
                    .as_ref()
                    .is_some_and(|span| span.breadcrumb.ends_with("gone"))
        }),
        "gone must be classified: {alignments:?}"
    );
}

#[test]
fn reconcile_detects_rename_by_normalized_body() {
    let before = "fn before_name() {\n    work();\n    work();\n}\n";
    let after = "fn after_name() {\n    work();\n    work();\n}\n";
    let hunks = vec![Hunk {
        old_start: 1,
        old_count: 4,
        new_start: 1,
        new_count: 4,
        text: "@@ -1,4 +1,4 @@\n-fn before_name() {\n+fn after_name() {\n     work();\n     work();\n}\n"
            .to_string(),
        start_offset: 0,
        end_offset: 0,
    }];
    let alignments = reconcile_alignments(
        &hunks,
        before,
        after,
        "src/x.rs",
        "src/x.rs",
        false,
        ChangeKind::Modified,
    );
    let renamed = alignments
        .iter()
        .find(|alignment| alignment.change_kind == ChangeKind::Renamed)
        .expect("same-body rename must be detected");
    assert!(renamed
        .old_span
        .as_ref()
        .is_some_and(|span| span.name == "before_name"));
    assert!(renamed
        .new_span
        .as_ref()
        .is_some_and(|span| span.name == "after_name"));
}

#[test]
fn oversized_hunk_splits_within_the_limit() {
    let mut line = String::from("+    let value = ");
    for index in 0..2_000 {
        line.push_str(&format!("data{index}_"));
    }
    line.push('\n');
    let hunk_text = format!("@@ -1,2 +1,2 @@\n{line}");
    let pieces = split_hunk_text(&hunk_text, MAX_TOKENS - 20);
    assert!(pieces.len() > 1);
    assert!(pieces
        .iter()
        .all(|piece| estimate_tokens(piece) <= MAX_TOKENS - 20));
    assert_eq!(pieces.concat(), hunk_text);
}

#[test]
fn split_hunk_text_handles_long_lines_without_looping() {
    let text = "+".repeat(5_000);
    let pieces = split_hunk_text(&text, 100);
    assert!(pieces.len() > 1);
    assert!(pieces.iter().all(|piece| estimate_tokens(piece) <= 101));
    assert_eq!(pieces.concat(), text);
}

#[test]
fn push_units_bounds_every_unit() {
    let atoms = vec![ParsedAtom {
        kind: AtomKind::Commit,
        parent_index: None,
        ordinal: 0,
        start_offset: 0,
        end_offset: 3,
        text: "msg".to_string(),
        content_hash: "h".to_string(),
        breadcrumb: "git:abc".to_string(),
        metadata: serde_json::json!({}),
    }];
    let mut units = Vec::new();
    let oversized = "+".repeat(MAX_TOKENS * 8);
    push_units(
        PushContext {
            output: &mut units,
            atoms: &atoms,
            atom_indices: vec![0],
            header: "commit abc subject\n\npath\n\n".to_string(),
            routing: "source: git_change".to_string(),
            metadata: serde_json::json!({}),
            anchors: Vec::new(),
        },
        &[oversized],
    );
    assert!(units.len() > 1);
    assert!(units.iter().all(|unit| unit.token_count <= MAX_TOKENS));
    assert!(units
        .iter()
        .enumerate()
        .all(|(index, unit)| { unit.metadata["part"] == serde_json::json!(index + 1) }));
}

#[test]
fn push_units_respects_header_budget() {
    let atoms = vec![ParsedAtom {
        kind: AtomKind::Commit,
        parent_index: None,
        ordinal: 0,
        start_offset: 0,
        end_offset: 3,
        text: "msg".to_string(),
        content_hash: "h".to_string(),
        breadcrumb: "git:abc".to_string(),
        metadata: serde_json::json!({}),
    }];
    let header = format!(
        "commit {} {}\n\n{}\n\n",
        "a".repeat(200),
        "b".repeat(200),
        "c".repeat(200)
    );
    let texts: Vec<String> = (0..10)
        .map(|index| {
            format!(
                "@@ -{index},1 +{index},1 @@\n context\n+{}\n",
                "x".repeat(600)
            )
        })
        .collect();
    let mut units = Vec::new();
    push_units(
        PushContext {
            output: &mut units,
            atoms: &atoms,
            atom_indices: vec![0],
            header,
            routing: "source: git_change".to_string(),
            metadata: serde_json::json!({}),
            anchors: Vec::new(),
        },
        &texts,
    );
    assert!(
        units.len() > 1,
        "medium hunks with a large header must split into parts"
    );
    assert!(units.iter().all(|unit| unit.token_count <= MAX_TOKENS));
}

#[test]
fn rename_index_collapses_pairwise_body_work() {
    // Audit finding 1 (run 20260830195149-6f1a96a5): doubling G must grow
    // the rename matcher's length gates + body compares < 2.2x and span-body
    // bytes < 3x (closed forms were G^2 / G^3 / G^2 x S_f before repair).
    // Budget (advisory, machine-dependent): reconcile wall <= 20 ms at
    // G=512 dense-packed (12.27 s before).
    use std::sync::atomic::Ordering;
    let run = |g: usize| {
        let mut before = String::new();
        let mut after = String::new();
        for i in 0..g {
            before.push_str(&format!(
                "fn sym_{i}() {{\n    step_{i}_a();\n    step_{i}_b();\n}}\n\n"
            ));
            after.push_str(&format!(
                "fn renamed_{i}() {{\n    step_{i}_a();\n    step_{i}_b();\n}}\n\n"
            ));
        }
        let old_count = before.lines().count() as u32;
        let new_count = after.lines().count() as u32;
        let mut text = format!("@@ -1,{old_count} +1,{new_count} @@\n");
        // Only the `fn` name lines change; bodies stay as context. Changing
        // every line would trip the whole-file fallback (covers_file) on the
        // new side and collapse all groups into one.
        for (old_line, new_line) in before.lines().zip(after.lines()) {
            if old_line.starts_with("fn ") {
                text.push_str("-");
                text.push_str(old_line);
                text.push('\n');
                text.push_str("+");
                text.push_str(new_line);
                text.push('\n');
            } else {
                text.push_str(" ");
                text.push_str(old_line);
                text.push('\n');
            }
        }
        let hunks = vec![Hunk {
            old_start: 1,
            old_count,
            new_start: 1,
            new_count,
            text,
            start_offset: 0,
            end_offset: 0,
        }];
        let len_before = super::align::RENAME_LEN_CHECKS.load(Ordering::Relaxed);
        let body_before = super::align::RENAME_BODY_COMPARES.load(Ordering::Relaxed);
        let bytes_before = super::align::SPAN_BODY_BYTES.load(Ordering::Relaxed);
        let alignments = reconcile_alignments(
            &hunks,
            &before,
            &after,
            "src/x.rs",
            "src/x.rs",
            false,
            ChangeKind::Modified,
        );
        let renamed = alignments
            .iter()
            .filter(|alignment| alignment.change_kind == ChangeKind::Renamed)
            .count();
        assert_eq!(renamed, g, "every renamed symbol must be classified");
        assert_eq!(renamed, g, "every renamed symbol must be classified");
        (
            super::align::RENAME_LEN_CHECKS.load(Ordering::Relaxed) - len_before,
            super::align::RENAME_BODY_COMPARES.load(Ordering::Relaxed) - body_before,
            super::align::SPAN_BODY_BYTES.load(Ordering::Relaxed) - bytes_before,
        )
    };
    let (small_len, small_body, small_bytes) = run(24);
    let (large_len, large_body, large_bytes) = run(48);
    let small_ops = small_len + small_body;
    let large_ops = large_len + large_body;
    assert!(
        (large_ops as f64) < 2.2 * small_ops as f64,
        "len checks + body compares grew {small_ops} -> {large_ops} (bound 2.2x)"
    );
    assert!(
        (large_bytes as f64) < 3.0 * small_bytes as f64,
        "span-body bytes grew {small_bytes} -> {large_bytes} (bound 3x)"
    );
}

#[test]
fn spawn_count_is_flat_in_files_per_commit() {
    // Audit finding 3 (run 20260830195149-6f1a96a5): spawns per commit stay
    // constant in F (was exactly 4F + 1). Invariant: spawns/run <= 4C + k0.
    // Budget (advisory): 128-file commit spawn wall <= 50 ms (790 ms before).
    use std::sync::atomic::Ordering;
    let run = |files: usize| -> u64 {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "guard@test"]);
        git(&["config", "user.name", "guard"]);
        for i in 0..files {
            std::fs::write(
                dir.path().join(format!("m{i}.rs")),
                format!("fn f{i}() {{\n    let _ = {i};\n}}\n"),
            )
            .unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "batch"]);
        let head = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let commit = super::history::CommitRef {
            oid: head.trim().to_string(),
            timestamp: 0,
            message: "batch".to_string(),
            content_hash: "guard".to_string(),
        };
        let before = super::history::GIT_SPAWNS.load(Ordering::Relaxed);
        let units = super::emit::ingest_commit(dir.path(), &commit).unwrap();
        assert!(!units.is_empty(), "a {files}-file commit must build units");
        super::history::GIT_SPAWNS.load(Ordering::Relaxed) - before
    };
    let small = run(4);
    let large = run(32);
    assert_eq!(
        small, large,
        "spawn count must not grow with files per commit"
    );
    assert!(
        large <= 8,
        "expected <= 8 spawns for one commit, got {large}"
    );
}

#[test]
fn batch_stream_stays_aligned_across_nonblob_replies() {
    use std::io::Cursor;

    let mut stream = Cursor::new(
        b"<a> commit 8\nbodyline\n<b> blob 6\nworld!\n<c> missing\n".to_vec(),
    );
    let super::history::Reply::Empty = super::history::decode_reply(&mut stream) else {
        panic!("non-blob reply must drain its frame and return Empty");
    };
    let super::history::Reply::Blob(text) = super::history::decode_reply(&mut stream) else {
        panic!("the blob reply after a drained frame must decode");
    };
    assert_eq!(text, "world!");
    let super::history::Reply::Empty = super::history::decode_reply(&mut stream) else {
        panic!("missing replies stay aligned without draining");
    };
    assert_eq!(
        stream.position() as usize,
        b"<a> commit 8\nbodyline\n<b> blob 6\nworld!\n<c> missing\n".len(),
        "every reply byte is consumed exactly once"
    );
}

#[test]
fn unparsable_size_marks_stream_broken() {
    use std::io::Cursor;

    let mut stream = Cursor::new(b"<a> commit notanumber\nwhatever\n".to_vec());
    let super::history::Reply::Broken = super::history::decode_reply(&mut stream) else {
        panic!("an unparsable size means framing is already lost");
    };
}
