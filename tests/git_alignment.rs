use std::path::Path;
use std::process::Command;

use snoop::core::SourceKind;
use snoop::inference::MockEmbedder;
use snoop::ingest::index_repository_bounded;
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

const AUTH_SEED: &str = "pub fn refresh_session() {\n    validate();\n}\n\nfn validate() {\n    true\n}\n\nfn rotate() {\n    rotate_key();\n}\n";
const AUTH_PRUNED: &str =
    "pub fn refresh_session() {\n    validate();\n}\n\nfn validate() {\n    true\n}\n";
const AUTH_VERIFIED: &str =
    "pub fn refresh_session() {\n    verify_session();\n}\n\nfn verify_session() {\n    true\n}\n";

const STORE_SYNC: &str = "void Store::flush() {\n    sync();\n}\n";
const STORE_VERIFY: &str = "void Store::flush() {\n    verify();\n}\n";

const WORKER_DRAIN: &str = "namespace Acme;\n\npublic class Worker\n{\n    public void drain_queue()\n    {\n        poll();\n    }\n}\n";
const WORKER_VERIFY: &str =
    "namespace Acme;\n\npublic class Worker\n{\n    public void drain_queue()\n    {\n        verify();\n    }\n}\n";

const GEOM_PLAIN: &str = "double scale_vector(double v, double s) {\n    return v * s;\n}\n";
const GEOM_SCALED: &str = "double scale_vector(double v, double s) {\n    return v * s * 2.0;\n}\n";

fn alignment_repo(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/auth.rs"), AUTH_SEED).unwrap();
    std::fs::write(root.join("src/tool.py"), "def load():\n    return 1\n").unwrap();
    std::fs::write(root.join("notes.txt"), "note one\n").unwrap();
    std::fs::write(root.join("src/store.cpp"), STORE_SYNC).unwrap();
    std::fs::write(root.join("src/worker.cs"), WORKER_DRAIN).unwrap();
    std::fs::write(root.join("src/geom.c"), GEOM_PLAIN).unwrap();
    git(root, &["init", "--quiet"]);
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "seed"]);

    std::fs::write(
        root.join("src/tool.py"),
        "def load():\n    total = 1\n    return total\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "grow load"]);

    std::fs::write(root.join("src/store.cpp"), STORE_VERIFY).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "rework flush body"]);

    std::fs::write(root.join("src/worker.cs"), WORKER_VERIFY).unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &["commit", "--quiet", "-m", "rework drain_queue body"],
    );

    std::fs::write(root.join("src/geom.c"), GEOM_SCALED).unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &["commit", "--quiet", "-m", "rework scale_vector body"],
    );

    std::fs::write(root.join("src/auth.rs"), AUTH_PRUNED).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "drop rotate"]);

    std::fs::write(root.join("src/auth.rs"), AUTH_VERIFIED).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "clarify validate"]);

    git(root, &["mv", "src/auth.rs", "src/session.rs"]);
    git(root, &["commit", "--quiet", "-m", "rename auth module"]);

    std::fs::write(root.join("notes.txt"), "note one\nnote two\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "extend notes"]);
}

struct Change {
    strategy: String,
    change_kind: String,
    path: String,
    old_path: Option<String>,
    symbol_id: Option<String>,
    old_symbol: Option<String>,
    language: Option<String>,
    evidence: String,
    routing: String,
}

fn git_changes(store: &Store) -> Vec<Change> {
    let mut changes = Vec::new();
    for id in store.unit_ids().unwrap() {
        let unit = store.unit_by_id(id).unwrap().unwrap();
        if unit.source_kind != SourceKind::GitCommit {
            continue;
        }
        changes.push(Change {
            strategy: unit.metadata["strategy"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            change_kind: unit.metadata["change_kind"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            path: unit.metadata["path"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            old_path: unit.metadata["old_path"].as_str().map(str::to_string),
            symbol_id: unit.metadata["symbol_id"].as_str().map(str::to_string),
            old_symbol: unit.metadata["old_symbol"].as_str().map(str::to_string),
            language: unit.metadata["language"].as_str().map(str::to_string),
            evidence: unit.evidence_text.clone(),
            routing: unit.routing_text.clone(),
        });
    }
    changes
}

fn indexed_changes(directory: &tempfile::TempDir) -> Vec<Change> {
    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    git_changes(&store)
}

#[test]
fn cpp_edits_align_to_qualified_methods() {
    let directory = tempfile::tempdir().unwrap();
    alignment_repo(directory.path());
    let changes = indexed_changes(&directory);
    let cpp = changes
        .iter()
        .find(|change| change.path == "src/store.cpp" && change.symbol_id.is_some())
        .expect("symbol-aligned unit for the cpp edit");
    assert_eq!(cpp.language.as_deref(), Some("cpp"));
    assert_eq!(cpp.change_kind, "modified");
    assert_eq!(
        cpp.symbol_id.as_deref(),
        Some("src/store.cpp > Store::flush")
    );
    assert!(cpp.evidence.contains("verify()"));
}

#[test]
fn csharp_edits_align_to_qualified_methods() {
    let directory = tempfile::tempdir().unwrap();
    alignment_repo(directory.path());
    let changes = indexed_changes(&directory);
    let csharp = changes
        .iter()
        .find(|change| change.path == "src/worker.cs" && change.symbol_id.is_some())
        .expect("symbol-aligned unit for the csharp edit");
    assert_eq!(csharp.language.as_deref(), Some("csharp"));
    assert_eq!(csharp.change_kind, "modified");
    assert_eq!(
        csharp.symbol_id.as_deref(),
        Some("src/worker.cs > Acme > Worker > drain_queue")
    );
    assert!(csharp.evidence.contains("verify();"));
}

#[test]
fn c_edits_align_to_functions() {
    let directory = tempfile::tempdir().unwrap();
    alignment_repo(directory.path());
    let changes = indexed_changes(&directory);
    let c = changes
        .iter()
        .find(|change| change.path == "src/geom.c" && change.symbol_id.is_some())
        .expect("symbol-aligned unit for the c edit");
    assert_eq!(c.language.as_deref(), Some("c"));
    assert_eq!(c.change_kind, "modified");
    assert_eq!(c.symbol_id.as_deref(), Some("src/geom.c > scale_vector"));
    assert!(c.evidence.contains("* 2.0"));
}

#[test]
fn deleted_symbols_carry_deletion_metadata() {
    let directory = tempfile::tempdir().unwrap();
    alignment_repo(directory.path());
    let changes = indexed_changes(&directory);
    let deleted = changes
        .iter()
        .find(|change| change.change_kind == "deleted" && change.symbol_id.is_some())
        .expect("deleted symbol unit for rotate");
    assert_eq!(deleted.strategy, "symbol");
    assert_eq!(deleted.symbol_id.as_deref(), Some("src/auth.rs > rotate"));
    assert_eq!(deleted.old_symbol.as_deref(), Some("src/auth.rs > rotate"));
    assert!(deleted.evidence.contains("rotate_key"));
}

#[test]
fn renamed_symbols_match_across_names() {
    let directory = tempfile::tempdir().unwrap();
    alignment_repo(directory.path());
    let changes = indexed_changes(&directory);
    let renamed = changes
        .iter()
        .find(|change| change.change_kind == "renamed")
        .expect("renamed symbol unit for validate -> verify_session");
    assert_eq!(renamed.strategy, "symbol");
    assert_eq!(
        renamed.symbol_id.as_deref(),
        Some("src/auth.rs > verify_session")
    );
    assert_eq!(
        renamed.old_symbol.as_deref(),
        Some("src/auth.rs > validate")
    );
}

#[test]
fn pure_file_renames_become_moved_units() {
    let directory = tempfile::tempdir().unwrap();
    alignment_repo(directory.path());
    let changes = indexed_changes(&directory);
    let moved = changes
        .iter()
        .find(|change| change.change_kind == "moved")
        .expect("moved unit for the pure file rename");
    assert_eq!(moved.strategy, "file");
    assert_eq!(moved.path, "src/session.rs");
    assert_eq!(moved.old_path.as_deref(), Some("src/auth.rs"));
    assert!(moved.symbol_id.is_none());
    assert!(moved.routing.contains("changed file: src/session.rs"));
}

#[test]
fn unsupported_files_fall_back_to_file_units_with_patch_evidence() {
    let directory = tempfile::tempdir().unwrap();
    alignment_repo(directory.path());
    let changes = indexed_changes(&directory);
    let note = changes
        .iter()
        .find(|change| change.path == "notes.txt" && change.change_kind == "modified")
        .expect("file-level unit for the notes.txt edit");
    assert_eq!(note.strategy, "file");
    assert!(note.symbol_id.is_none());
    assert!(note.evidence.contains("+note two"));
}

#[test]
fn git_symbol_ids_match_code_unit_identities() {
    let directory = tempfile::tempdir().unwrap();
    alignment_repo(directory.path());
    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    let mut code_identity: Option<String> = None;
    for id in store.unit_ids().unwrap() {
        let unit = store.unit_by_id(id).unwrap().unwrap();
        if unit.source_kind != SourceKind::Code {
            continue;
        }
        for line in unit.routing_text.lines() {
            if let Some(symbol) = line.strip_prefix("symbol: ") {
                if symbol == "src/tool.py > load" {
                    code_identity = Some(symbol.to_string());
                }
            }
        }
    }
    let changes = git_changes(&store);
    let git_change = changes
        .iter()
        .find(|change| change.symbol_id.as_deref() == Some("src/tool.py > load"))
        .expect("symbol-aligned git unit for the python edit");
    assert_eq!(git_change.language.as_deref(), Some("python"));
    assert_eq!(git_change.change_kind, "modified");
    assert!(git_change.evidence.contains("total = 1"));
    let git_identity = git_change.symbol_id.clone();
    assert_eq!(code_identity, git_identity);
}

#[test]
fn git_indexing_is_deterministic() {
    fn survey(directory: &tempfile::TempDir) -> Vec<(String, String, String)> {
        let mut store = Store::open_in_memory().unwrap();
        let embedder = MockEmbedder::new("mock-v1");
        let outcome =
            index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
        let mut rows = Vec::new();
        for id in store.unit_ids().unwrap() {
            let unit = store.unit_by_id(id).unwrap().unwrap();
            if unit.source_kind != SourceKind::GitCommit {
                continue;
            }
            rows.push((
                unit.routing_text.clone(),
                unit.evidence_text.clone(),
                unit.metadata.to_string(),
            ));
        }
        rows.sort();
        rows
    }

    let first = tempfile::tempdir().unwrap();
    alignment_repo(first.path());
    let second = tempfile::tempdir().unwrap();
    alignment_repo(second.path());
    assert_eq!(survey(&first), survey(&second));
}
