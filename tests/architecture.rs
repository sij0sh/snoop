//! Architecture invariants, enforced in CI.
//!
//! Two proven codebase-scaling defects are guarded here:
//!
//! 1. The incremental-ingest protocol (unchanged-skip, deadline bail,
//!    commit, tally) has exactly one implementation: `ingest_candidate` in
//!    src/ingest/mod.rs. Source families must become candidate producers,
//!    not protocol copies.
//! 2. Cross-module metadata contracts have exactly one owner: src/metadata.rs.
//!    Owned key literals must not reappear in other production modules; a
//!    contract change is then one edit the compiler traces.
//!
//! The scans cover production code only: files under src/, excluding test
//! modules (files named tests.rs, subtrees under a tests/ directory, and
//! inline `#[cfg(test)] mod ... { ... }` bodies).

use std::fs;
use std::path::{Path, PathBuf};

/// Keys whose names are unique to their metadata contract: any quoted
/// literal outside the owner is a leaked restatement.
const DISTINCTIVE_KEYS: &[&str] = &[
    "leading_context",
    "source_slices",
    "chunk_segments",
    "is_import",
    "git_tip",
];

/// Keys whose names also occur in unrelated domains (anchor kinds, MCP tool
/// schemas, output fields): only the metadata access pattern is guarded.
const ACCESS_ONLY_KEYS: &[&str] = &["symbol", "signature", "references", "timestamp"];

fn walk_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("src/ directory exists") {
        let entry = entry.expect("readable directory entry");
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "tests") {
                continue;
            }
            walk_rust_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// Removes inline `#[cfg(test)] mod <name> { ... }` bodies. Declaration
/// form (`mod tests;`) has no body and passes through untouched.
fn strip_inline_tests(content: &str) -> String {
    let mut output = String::with_capacity(content.len());
    let mut lines = content.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() == "#[cfg(test)]" {
            if let Some(next) = lines.peek() {
                let next = next.trim();
                if next.starts_with("mod ") && next.ends_with('{') {
                    let mut depth = 1usize;
                    for inner in lines.by_ref() {
                        depth += inner.matches('{').count();
                        depth -= inner.matches('}').count();
                        if depth == 0 {
                            break;
                        }
                    }
                    continue;
                }
            }
        }
        output.push_str(line);
        output.push('\n');
    }
    output
}

/// Production sources as (module-relative path, content) pairs, with the
/// owner module filter applied by callers that need it.
fn production_sources() -> Vec<(String, String)> {
    let mut files = Vec::new();
    walk_rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    files
        .into_iter()
        .filter(|path| !path.file_stem().is_some_and(|stem| stem == "tests"))
        .map(|path| {
            let label = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let content = strip_inline_tests(&fs::read_to_string(&path).expect("readable source"));
            (label, content)
        })
        .collect()
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn ingest_protocol_has_one_implementation() {
    let sources = production_sources();
    let total = |needle: &str| -> (usize, String) {
        let mut sites = Vec::new();
        let mut total = 0;
        for (path, content) in &sources {
            let hits = count(content, needle);
            if hits > 0 {
                sites.push(format!("{path} x{hits}"));
            }
            total += hits;
        }
        (total, sites.join(", "))
    };

    let (unchanged_checks, sites) = total(".source_by_locator(");
    assert_eq!(
        unchanged_checks, 1,
        "the unchanged-comparison must appear only in ingest_candidate (src/ingest/mod.rs); found: {sites}"
    );
    let (commits, sites) = total(".commit_source(");
    assert_eq!(
        commits, 1,
        "commit_source must be called only in ingest_candidate (src/ingest/mod.rs); found: {sites}"
    );
    let (timeout_writes, sites) = total("timed_out = true");
    assert_eq!(
        timeout_writes, 1,
        "the ingest deadline bail must write timed_out in exactly one place (ingest_candidate); found: {sites}"
    );
}

#[test]
fn metadata_contracts_have_one_owner() {
    let owner = production_sources()
        .into_iter()
        .find(|(path, _)| path.ends_with("src/metadata.rs"))
        .expect("src/metadata.rs owns the cross-module metadata contracts");

    for key in DISTINCTIVE_KEYS.iter().chain(ACCESS_ONLY_KEYS) {
        assert!(
            count(&owner.1, &format!("\"{key}\"")) >= 1,
            "owner module no longer defines the {key:?} contract"
        );
    }

    let others: Vec<_> = production_sources()
        .into_iter()
        .filter(|(path, _)| !path.ends_with("src/metadata.rs"))
        .collect();

    for key in DISTINCTIVE_KEYS {
        for (path, content) in &others {
            assert_eq!(
                count(content, &format!("\"{key}\"")),
                0,
                "metadata key {key:?} literal leaked into {path}; route the site through crate::metadata"
            );
        }
    }

    for key in DISTINCTIVE_KEYS.iter().chain(ACCESS_ONLY_KEYS) {
        for (path, content) in &others {
            assert_eq!(
                count(content, &format!("metadata[\"{key}\"]")),
                0,
                "metadata key {key:?} read/written directly in {path}; use the owner in crate::metadata"
            );
        }
    }
}
