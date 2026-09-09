//! Retrieval regression fixtures: three negative cases plus positive
//! controls. Negative cases pin desired end-state behavior; N2 stays
//! ignored until Phase D/E gate supporting lanes and fill.

use super::super::*;
use crate::core::{hash_segments, AnchorKind, BuiltAnchor, BuiltUnit, SourceKind, UnitKind};
use crate::ingest::units::estimate_tokens;
use crate::store::SourceIngest;

fn test_options() -> QueryOptions {
    QueryOptions {
        // Lexical channels only: with vectors on, a tiny fixture would put
        // every unit in the vector top-n and hide the provenance path.
        channels: QueryChannels::for_embedder(None),
        top_n: 25,
        max_tokens: 6_000,
        diagnostics: true,
        ..QueryOptions::default()
    }
}

fn test_code_unit(evidence: &str, file: &str, anchor_symbol: Option<&str>) -> BuiltUnit {
    let mut anchors = vec![BuiltAnchor {
        kind: AnchorKind::File,
        value: file.to_string(),
        relationship: "defines".to_string(),
    }];
    if let Some(symbol) = anchor_symbol {
        anchors.push(BuiltAnchor {
            kind: AnchorKind::Symbol,
            value: symbol.to_string(),
            relationship: "defines".to_string(),
        });
    }
    BuiltUnit {
        kind: UnitKind::Code,
        evidence_text: evidence.to_string(),
        routing_text: String::new(),
        token_count: estimate_tokens(evidence),
        content_hash: hash_segments(&[evidence]),
        metadata: serde_json::json!({}),
        anchors,
    }
}

/// Mirror of `ingest::git::emit` unit shape: shared commit header plus
/// per-file breadcrumb, routing in `git_routing` shape, commit/file/symbol
/// anchors, path metadata.
fn git_unit(
    sha: &str,
    subject: &str,
    path: &str,
    symbol: Option<&str>,
    body: &str,
) -> BuiltUnit {
    let short = sha.get(..8).unwrap_or(sha);
    let breadcrumb = match symbol {
        Some(name) => format!("{path} > {name}"),
        None => path.to_string(),
    };
    let evidence = format!("commit {short} {subject}\n\n{breadcrumb}\n\n{body}");
    let routing = format!(
        "source: git_change\ncommit: {short}\nmessage: {subject}\nchanged file: {path}\nchanged symbol: {}",
        symbol.unwrap_or("-")
    );
    let mut anchors = vec![
        BuiltAnchor {
            kind: AnchorKind::Commit,
            value: sha.to_string(),
            relationship: "part_of".to_string(),
        },
        BuiltAnchor {
            kind: AnchorKind::File,
            value: path.to_string(),
            relationship: "changes".to_string(),
        },
    ];
    if let Some(name) = symbol {
        anchors.push(BuiltAnchor {
            kind: AnchorKind::Symbol,
            value: name.to_string(),
            relationship: "changes".to_string(),
        });
    }
    let mut metadata = serde_json::json!({"commit": sha, "path": path});
    if let Some(name) = symbol {
        metadata["symbol_id"] = serde_json::json!(name);
    }
    BuiltUnit {
        kind: UnitKind::Git,
        evidence_text: evidence.clone(),
        routing_text: routing,
        token_count: estimate_tokens(&evidence),
        content_hash: hash_segments(&[&evidence]),
        metadata,
        anchors,
    }
}

fn git_commit_source(store: &mut Store, oid: &str, units: &[BuiltUnit]) {
    let locator = format!("git:{oid}");
    store
        .commit_source(SourceIngest {
            kind: SourceKind::GitCommit,
            locator: &locator,
            content_hash: &hash_segments(&[&locator, &units.len().to_string()]),
            modified_at: None,
            metadata: serde_json::json!({}),
            units,
        })
        .unwrap();
}

fn markdown_source(store: &mut Store, locator: &str, units: &[BuiltUnit]) {
    store
        .commit_source(SourceIngest {
            kind: SourceKind::Markdown,
            locator,
            content_hash: &hash_segments(&[locator, &units.len().to_string()]),
            modified_at: None,
            metadata: serde_json::json!({}),
            units,
        })
        .unwrap();
}

fn markdown_unit(evidence: &str) -> BuiltUnit {
    BuiltUnit {
        kind: UnitKind::Prose,
        evidence_text: evidence.to_string(),
        routing_text: String::new(),
        token_count: estimate_tokens(evidence),
        content_hash: hash_segments(&[evidence]),
        metadata: serde_json::json!({}),
        anchors: Vec::new(),
    }
}

fn git_locators(report: &QueryReport) -> Vec<String> {
    report
        .packet
        .items
        .iter()
        .filter(|item| item.source_kind == SourceKind::GitCommit)
        .map(|item| item.source_locator.clone())
        .collect()
}

// N1: repeated same-commit siblings. Desired: at most 2 items from one
// commit. Fails while the per-source cap admits 3; goes green in Phase B.
#[test]
fn same_commit_siblings_are_capped_per_commit() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    let sha = "9fcf66bc9131162d6c94c85062701858affd4fd1";
    git_commit_source(
        &mut store,
        sha,
        &[
            git_unit(sha, "Start using auto-formatters", "bandit/core/utils.py", Some("ConfigError"), "@@ -95 +93 @@ class ConfigError"),
            git_unit(sha, "Start using auto-formatters", "bandit/core/extension.py", Some("Extension"), "@@ -12 +12 @@ class Extension"),
            git_unit(sha, "Start using auto-formatters", "bandit/core/test.py", Some("Tester"), "@@ -40 +40 @@ class Tester"),
        ],
    );
    let report = query(&store, None, "why start using auto formatters", &test_options()).unwrap();
    let from_sha = git_locators(&report)
        .iter()
        .filter(|locator| locator.contains(sha))
        .count();
    assert!(from_sha <= 2, "one commit rendered {from_sha} items");
}

// N2: greenfield query against realistic git noise. The junk units match the
// generic word in both evidence and routing text, like emitted units do.
#[test]
fn greenfield_query_keeps_strong_code_evidence() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    store
        .commit_source(SourceIngest {
            kind: SourceKind::Code,
            locator: "bandit/core/cache.py",
            content_hash: &hash_segments(&["bandit/core/cache.py"]),
            modified_at: None,
            metadata: serde_json::json!({}),
            units: &[test_code_unit(
                "incremental cache design: cache_hits and cache_misses counters with force-rescan and warm start support",
                "bandit/core/cache.py",
                Some("IncrementalCache"),
            )],
        })
        .unwrap();
    for index in 0..10 {
        let sha = format!("junk{index:04}");
        git_commit_source(
            &mut store,
            &sha,
            &[git_unit(
                &sha,
                "tune cache sizes",
                &format!("src/cache{index}.py"),
                None,
                "adjust default cache limits",
            )],
        );
    }
    let report = query(
        &store,
        None,
        "incremental cache cache_hits cache_misses force-rescan warm start",
        &test_options(),
    )
    .unwrap();
    assert!(
        report
            .packet
            .items
            .iter()
            .any(|item| item.source_locator == "bandit/core/cache.py"),
        "strong code evidence must survive"
    );
}

// N2 junk gate: supporting lanes (Phase D) and fill (Phase E) both require
// relevance, so generic single-concept commits reach neither.
#[test]
fn greenfield_query_excludes_generic_commit_noise() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    store
        .commit_source(SourceIngest {
            kind: SourceKind::Code,
            locator: "bandit/core/cache.py",
            content_hash: &hash_segments(&["bandit/core/cache.py"]),
            modified_at: None,
            metadata: serde_json::json!({}),
            units: &[test_code_unit(
                "incremental cache design: cache_hits and cache_misses counters with force-rescan and warm start support",
                "bandit/core/cache.py",
                Some("IncrementalCache"),
            )],
        })
        .unwrap();
    for index in 0..10 {
        let sha = format!("junk{index:04}");
        git_commit_source(
            &mut store,
            &sha,
            &[git_unit(
                &sha,
                "tune cache sizes",
                &format!("src/cache{index}.py"),
                None,
                "adjust default cache limits",
            )],
        );
    }
    let report = query(
        &store,
        None,
        "incremental cache cache_hits cache_misses force-rescan warm start",
        &test_options(),
    )
    .unwrap();
    assert!(git_locators(&report).is_empty(), "generic cache commits admitted");
}

// N3: translated sibling docs. Desired: units from exactly one locale
// source. Fails while siblings flood channels; goes green in Phase C.
#[test]
fn translated_sibling_docs_collapse_to_one_source() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    // Translated siblings share untranslated identifiers (HEAD, router) in
    // code spans while prose differs, so content hashes stay distinct.
    let bodies = [
        ("en", "HEAD requests route through the application router automatically"),
        ("ja", "HEAD \u{30ea}\u{30af}\u{30a8}\u{30b9}\u{30c8}\u{306f} application router \u{3092}\u{7d4c}\u{7531}\u{3057}\u{307e}\u{3059}"),
        ("zh", "HEAD \u{8bf7}\u{6c42}\u{901a}\u{8fc7} application router \u{81ea}\u{52a8}\u{8def}\u{7531}"),
        ("zh-hant", "HEAD \u{8981}\u{6c42}\u{900f}\u{904e} application router \u{81ea}\u{52d5}\u{8def}\u{7531}"),
    ];
    for (locale, body) in bodies {
        let locator = format!("docs/{locale}/docs/tutorial/first-steps.md");
        markdown_source(&mut store, &locator, &[markdown_unit(body)]);
    }
    let report = query(&store, None, "how do HEAD requests route", &test_options()).unwrap();
    let mut sources: Vec<String> = report
        .packet
        .items
        .iter()
        .filter(|item| item.source_kind == SourceKind::Markdown)
        .map(|item| item.source_locator.clone())
        .collect();
    sources.sort();
    sources.dedup();
    assert!(sources.len() <= 1, "translated siblings admitted: {sources:?}");
}

// P1: evidence-only body match must survive lane gating and fill.
#[test]
fn evidence_only_body_match_is_admitted() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    store
        .commit_source(SourceIngest {
            kind: SourceKind::Code,
            locator: "src/body.rs",
            content_hash: &hash_segments(&["src/body.rs"]),
            modified_at: None,
            metadata: serde_json::json!({}),
            units: &[test_code_unit(
                "the frobnicate routine normalizes widget handles",
                "src/body.rs",
                None,
            )],
        })
        .unwrap();
    let report = query(&store, None, "where is frobnicate handled", &test_options()).unwrap();
    assert!(
        report
            .packet
            .items
            .iter()
            .any(|item| item.source_locator == "src/body.rs"),
        "evidence-only hit dropped"
    );
}

// P2: evolution queries retrieve breadth of history, never a quota.
#[test]
fn evolution_query_retrieves_distinct_commits() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    for index in 0..5 {
        let sha = format!("evol{index:04}");
        git_commit_source(
            &mut store,
            &sha,
            &[git_unit(
                &sha,
                &format!("retry backoff attempt {index}"),
                "src/retry.rs",
                Some("retry_with_backoff"),
                "adjust retry delay computation",
            )],
        );
    }
    let report = query(
        &store,
        None,
        "when was retry introduced show history",
        &test_options(),
    )
    .unwrap();
    let mut oids = git_locators(&report);
    oids.sort();
    oids.dedup();
    assert!(oids.len() >= 3, "evolution recall too narrow: {oids:?}");
}

// P3: a two-file commit keeps both hunks.
#[test]
fn multi_file_commit_keeps_both_hunks() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    let sha = "multifile0001";
    git_commit_source(
        &mut store,
        sha,
        &[
            git_unit(sha, "rotate token before validation", "src/auth.rs", Some("rotate_token"), "call rotate_token first"),
            git_unit(sha, "rotate token before validation", "src/session.rs", Some("refresh_session"), "refresh_session rotates token"),
        ],
    );
    let report = query(&store, None, "rotate token before validation", &test_options()).unwrap();
    let count = git_locators(&report)
        .iter()
        .filter(|locator| locator.contains(sha))
        .count();
    assert_eq!(count, 2, "second hunk of the same commit dropped");
}

// P4: anchor expansion preserves relational history with weak lexical overlap.
#[test]
fn anchor_expansion_preserves_weak_lexical_history() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    store
        .commit_source(SourceIngest {
            kind: SourceKind::Code,
            locator: "src/auth.rs",
            content_hash: &hash_segments(&["src/auth.rs"]),
            modified_at: None,
            metadata: serde_json::json!({}),
            units: &[test_code_unit(
                "refresh_session validates the token then rotates it",
                "src/auth.rs",
                Some("refresh_session"),
            )],
        })
        .unwrap();
    let sha = "expand0001";
    git_commit_source(
        &mut store,
        sha,
        &[git_unit(
            sha,
            "scheduler tweaks",
            "src/sched.rs",
            Some("refresh_session"),
            "unrelated scheduler tweaks touching the session helper",
        )],
    );
    let report = query(
        &store,
        None,
        "how does refresh_session handle token rotation",
        &test_options(),
    )
    .unwrap();
    let debug = report.debug.unwrap();
    let expanded = debug.items.iter().any(|item| {
        item.selected_because.iter().any(|reason| {
            matches!(
                reason,
                crate::core::SelectionReason::AnchorExpansion(_, value, _)
                    if value == "refresh_session"
            )
        })
    });
    assert!(expanded, "symbol-anchored commit lost despite expansion");
}

// P5: rationale queries keep current code plus introducing history.
#[test]
fn rationale_query_keeps_code_and_history() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    store
        .commit_source(SourceIngest {
            kind: SourceKind::Code,
            locator: "src/auth.rs",
            content_hash: &hash_segments(&["src/auth.rs"]),
            modified_at: None,
            metadata: serde_json::json!({}),
            units: &[test_code_unit(
                "token refresh validates the token before rotation",
                "src/auth.rs",
                Some("refresh_session"),
            )],
        })
        .unwrap();
    markdown_source(
        &mut store,
        "docs/auth.md",
        &[markdown_unit("token refresh validates the token before rotation by design")],
    );
    let sha = "why0001";
    git_commit_source(
        &mut store,
        sha,
        &[git_unit(
            sha,
            "validate token before rotation",
            "src/auth.rs",
            Some("refresh_session"),
            "reorder validation ahead of rotation",
        )],
    );
    let report = query(
        &store,
        None,
        "why does token refresh validate before rotation",
        &test_options(),
    )
    .unwrap();
    let kinds: Vec<SourceKind> = report.packet.items.iter().map(|item| item.source_kind).collect();
    assert!(kinds.contains(&SourceKind::Code), "rationale query lost code");
    assert!(kinds.contains(&SourceKind::GitCommit), "rationale query lost history");
}
