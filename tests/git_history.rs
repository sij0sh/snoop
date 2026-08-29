use std::path::Path;
use std::process::Command;

use snoop::core::SourceKind;
use snoop::inference::MockEmbedder;
use snoop::ingest::index_repository_bounded;
use snoop::runtime::{query, QueryChannels, QueryOptions};
use snoop::store::Store;

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

fn fixture_repo(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session() {\n    validate();\n}\n\nfn validate() {}\n\nfn rotate() {}\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "# Auth\n\nSessions refresh.\n").unwrap();
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
            "prevent stale refresh-token reuse",
        ],
    );

    std::fs::write(root.join("notes.txt"), "plain text note\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "add plain text notes"]);
}

struct GitUnits {
    symbol_units: Vec<String>,
    file_fallback_paths: Vec<String>,
    removal_units: usize,
}

fn survey(store: &Store, repo: snoop::core::RepoId) -> GitUnits {
    let mut symbol_units = Vec::new();
    let mut file_fallback_paths = Vec::new();
    let mut removal_units = 0;
    for id in store.unit_ids(repo).unwrap() {
        let unit = store.unit_by_id(id).unwrap().unwrap();
        if unit.source_kind != SourceKind::GitCommit {
            continue;
        }
        assert!(unit.timestamp.is_some(), "commit units carry timestamps");
        assert!(unit.evidence_text.starts_with("commit "));
        assert!(unit.routing_text.contains("source: git_change"));
        match unit.metadata["strategy"].as_str() {
            Some("symbol") => symbol_units.push(
                unit.metadata["symbol"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ),
            Some("file") => file_fallback_paths.push(
                unit.metadata["path"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ),
            _ => {}
        }
        if unit.metadata["path"].as_str() == Some("notes.txt") {
            removal_units += 1;
        }
    }
    GitUnits {
        symbol_units,
        file_fallback_paths,
        removal_units,
    }
}

#[test]
fn git_history_indexes_symbol_units_and_falls_back() {
    let directory = tempfile::tempdir().unwrap();
    fixture_repo(directory.path());
    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome = index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let survey = survey(&store, outcome.repo_id);
    assert!(
        survey
            .symbol_units
            .iter()
            .any(|symbol| symbol.contains("refresh_session")),
        "expected a symbol-aligned unit for refresh_session, got {:?}",
        survey.symbol_units
    );
    assert!(
        survey
            .file_fallback_paths
            .contains(&"notes.txt".to_string()),
        "non-code changes fall back to file units, got {:?}",
        survey.file_fallback_paths
    );

    let report = query(
        &store,
        outcome.repo_id,
        Some(&embedder),
        "when was refresh token reuse prevented",
        &QueryOptions {
            channels: QueryChannels::for_embedder(Some(&embedder)),
            top_n: 10,
            max_tokens: 2_000,
        },
    )
    .unwrap();
    assert!(
        report
            .packet
            .items
            .iter()
            .any(|item| item.source_kind == SourceKind::GitCommit
                && item
                    .evidence_text
                    .contains("prevent stale refresh-token reuse")),
        "query must surface the commit unit with its diff"
    );

    let before = store.unit_ids(outcome.repo_id).unwrap();
    let second = index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(second.changed_sources, 0, "reindex processes nothing");
    assert_eq!(second.units_added, 0);
    assert_eq!(store.unit_ids(outcome.repo_id).unwrap(), before);
}

#[test]
fn deleted_file_commits_do_not_block_indexing() {
    let directory = tempfile::tempdir().unwrap();
    fixture_repo(directory.path());
    std::fs::remove_file(directory.path().join("notes.txt")).unwrap();
    git(directory.path(), &["add", "-A"]);
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "remove notes"],
    );

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome = index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    let survey = survey(&store, outcome.repo_id);
    assert!(
        survey.removal_units > 0,
        "deletion commits index as file units"
    );
}
