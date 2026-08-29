use std::path::Path;
use std::process::Command;

use snoop::core::SourceKind;
use snoop::inference::MockEmbedder;
use snoop::ingest::index_repository_bounded;
use snoop::runtime::{query, QueryChannels, QueryOptions};
use snoop::store::Store;

fn git(root: &Path, args: &[&str]) {
    for key in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
        std::env::set_var(key, "fixture");
    }
    for key in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
        std::env::set_var(key, "fixture@example.com");
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_DATE", "2026-08-20T12:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-08-20T12:00:00Z")
        .status()
        .expect("git spawns");
    assert!(status.success(), "git {args:?} failed");
}

fn fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session() {\n    validate();\n}\n\nfn validate() {}\n\nfn rotate() {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("docs/auth-plan.md"),
        "# Token rotation\n\n## Decision\n\n`refresh_session` validates before rotating `rotate_token`.\n\n## Order\n\nValidation is first.\n",
    )
    .unwrap();
    git(root, &["init", "--quiet"]);
    git(root, &["add", "."]);
    git(
        root,
        &["commit", "--quiet", "-m", "introduce session refresh"],
    );

    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session() {\n    validate();\n    rotate();\n}\n\nfn validate() {}\n\nfn rotate() {}\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &[
            "commit",
            "--quiet",
            "-m",
            "prevent stale refresh-token reuse in refresh_session",
        ],
    );
}

#[test]
fn anchors_are_emitted_for_every_source_kind() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome = index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let mut kinds = std::collections::HashSet::new();
    let mut confidence_seen = false;
    for id in store.unit_ids(outcome.repo_id).unwrap() {
        for (kind, relationship, anchor_id) in store.anchors_for_unit(id).unwrap() {
            kinds.insert(kind.clone());
            assert!(
                store
                    .anchor_value(outcome.repo_id, &kind, anchor_id)
                    .unwrap()
                    .is_some(),
                "anchor resolves to a value ({kind}, {relationship})"
            );
            confidence_seen = true;
        }
    }
    assert!(confidence_seen, "anchors exist");
    assert!(
        kinds.contains("file"),
        "file anchors emitted, got {kinds:?}"
    );
    assert!(
        kinds.contains("symbol"),
        "symbol anchors emitted, got {kinds:?}"
    );
    assert!(
        kinds.contains("commit"),
        "commit anchors emitted, got {kinds:?}"
    );

    let doc_mentions_symbol = store
        .units_for_anchor(outcome.repo_id, "symbol", "refresh_session", 32)
        .unwrap();
    assert!(
        doc_mentions_symbol.iter().any(|id| store
            .unit_by_id(*id)
            .unwrap()
            .is_some_and(|unit| unit.source_kind == SourceKind::Markdown)),
        "markdown unit mentioning refresh_session anchors to the symbol"
    );
}

#[test]
fn expansion_joins_code_docs_and_git_on_a_symbol() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome = index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let report = query(
        &store,
        outcome.repo_id,
        Some(&embedder),
        "refresh_session",
        &QueryOptions {
            channels: QueryChannels::for_embedder(Some(&embedder)),
            top_n: 10,
            max_tokens: 4_000,
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
        "code hit present: {kinds:?}"
    );
    assert!(
        report.packet.items.iter().any(|item| item
            .selected_because
            .iter()
            .any(|reason| matches!(reason, snoop::core::SelectionReason::AnchorExpansion(..)))),
        "at least one item selected via anchor expansion: {:?}",
        report
            .packet
            .items
            .iter()
            .map(|item| &item.selected_because)
            .collect::<Vec<_>>()
    );
    assert!(
        report
            .debug
            .expansion
            .iter()
            .any(|entry| entry.accepted && entry.anchor_kind == "symbol"),
        "--explain shows accepted symbol expansion"
    );
    let expanded_kinds: Vec<SourceKind> = report
        .packet
        .items
        .iter()
        .filter(|item| {
            item.selected_because
                .iter()
                .any(|reason| matches!(reason, snoop::core::SelectionReason::AnchorExpansion(..)))
        })
        .map(|item| item.source_kind)
        .collect();
    assert!(
        expanded_kinds
            .iter()
            .any(|kind| *kind == SourceKind::GitCommit || *kind == SourceKind::Markdown),
        "expansion surfaced docs or git evidence: {expanded_kinds:?}"
    );

    let why = query(
        &store,
        outcome.repo_id,
        Some(&embedder),
        "why does refresh session validate before rotation",
        &QueryOptions {
            channels: QueryChannels::for_embedder(Some(&embedder)),
            top_n: 10,
            max_tokens: 4_000,
        },
    )
    .unwrap();
    assert!(
        why.packet
            .items
            .iter()
            .any(|item| item.source_kind == SourceKind::Code),
        "code present"
    );
    assert!(
        why.packet
            .items
            .iter()
            .any(|item| item.source_kind == SourceKind::GitCommit),
        "commit evidence present for a why question: {:?}",
        why.packet
            .items
            .iter()
            .map(|item| item.source_kind)
            .collect::<Vec<_>>()
    );
}
