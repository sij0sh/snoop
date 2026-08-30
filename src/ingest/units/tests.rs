use super::*;
use crate::ingest::markdown::parse_markdown;

#[test]
fn markdown_units_are_reversible_and_have_routing_text() {
    let parsed = parse_markdown("# Auth\n\nRefresh tokens rotate.", "README");
    let units = build_units(&parsed.atoms, SourceKind::Markdown, "README.md");
    assert_eq!(units.len(), 1);
    assert!(!units[0].metadata["source_slices"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(units[0].routing_text.contains("heading: README > Auth"));
}

#[test]
fn routing_projection_is_deterministic_and_keeps_backticked_symbols() {
    let parsed = parse_markdown(
        "# Auth\n\nSee [auth design](docs/auth-plan.md), then use `TokenStore` in `src/auth.rs`.",
        "README",
    );
    let first = build_units(&parsed.atoms, SourceKind::Markdown, "README.md");
    let second = build_units(&parsed.atoms, SourceKind::Markdown, "README.md");
    assert_eq!(first, second);
    assert!(first[0].routing_text.contains("TokenStore"));
    assert!(first[0].routing_text.contains("src/auth.rs"));
    assert!(first[0].routing_text.contains("docs/auth-plan.md"));
    assert!(!first[0].routing_text.contains("design]("));
}

#[test]
fn code_routing_uses_the_qualified_symbol() {
    let atoms =
        crate::ingest::code::parse_code("impl Session { fn refresh(&self) {} }", "src/auth.rs")
            .unwrap();
    let units = build_units(&atoms, SourceKind::Code, "src/auth.rs");
    assert!(units.iter().any(|unit| unit
        .routing_text
        .contains("symbol: src/auth.rs > impl Session > refresh")));
    assert_eq!(atoms[0].text, "impl Session { fn refresh(&self) {} }");
}

#[test]
fn markdown_link_targets_change_canonical_units() {
    let first = parse_markdown("# Links\n\nSee [auth](docs/auth.md).", "README");
    let second = parse_markdown("# Links\n\nSee [auth](docs/session.md).", "README");
    let first_units = build_units(&first.atoms, SourceKind::Markdown, "README.md");
    let second_units = build_units(&second.atoms, SourceKind::Markdown, "README.md");
    assert_ne!(first_units[0].content_hash, second_units[0].content_hash);
    assert!(first_units[0].evidence_text.contains("docs/auth.md"));
}

#[test]
fn oversized_code_has_bounded_units_and_exact_ranges() {
    let long = "x".repeat(4_000);
    let source = format!("fn large() {{ let value = \"{long}\"; }}");
    let atoms = crate::ingest::code::parse_code(&source, "src/large.rs").unwrap();
    let units = build_units(&atoms, SourceKind::Code, "src/large.rs");
    assert!(units.len() > 1);
    assert!(units.iter().all(|unit| unit.token_count <= MAX_TOKENS));
    assert!(units
        .iter()
        .all(|unit| unit.metadata["source_slices"][0]["start_offset"].is_number()));
}

#[test]
fn oversized_prose_splits_within_the_limit() {
    let body = "A useful sentence. ".repeat(1_000);
    let parsed = parse_markdown(&format!("# Long\n\n{body}"), "README");
    let units = build_units(&parsed.atoms, SourceKind::Markdown, "README.md");
    assert!(units.len() > 1);
    assert!(units.iter().all(|unit| unit.token_count <= MAX_TOKENS));

    let near_limit = "x".repeat(3_190);
    let parsed = parse_markdown(&format!("# Heading\n\n{near_limit}"), "README");
    let units = build_units(&parsed.atoms, SourceKind::Markdown, "README.md");
    assert!(units.iter().all(|unit| unit.token_count <= MAX_TOKENS));
}

#[test]
fn containers_emit_shells_and_children_keep_their_units() {
    let source = "impl Session {\n    fn refresh(&self) { let refresh_token = 1; }\n    fn validate(&self) { let validation_code = 2; }\n}\n";
    let atoms = crate::ingest::code::parse_code(source, "src/auth.rs").unwrap();
    let units = build_units(&atoms, SourceKind::Code, "src/auth.rs");
    let shell = units
        .iter()
        .find(|unit| unit.metadata["unit_shape"] == "shell")
        .expect("container shell unit");
    assert!(shell.evidence_text.contains("fn refresh(&self)"));
    assert!(shell.evidence_text.contains("fn validate(&self)"));
    assert!(!shell.evidence_text.contains("refresh_token"));
    assert!(!shell.evidence_text.contains("validation_code"));
    assert_eq!(
        shell.metadata["elided_children"].as_array().map(Vec::len),
        Some(2)
    );
    let refresh = units
        .iter()
        .find(|unit| unit.routing_text.contains("> refresh"))
        .expect("child unit for refresh");
    assert!(refresh.evidence_text.contains("refresh_token"));
}

#[test]
fn attached_docs_deduplicate_and_trivial_comments_are_skipped() {
    let source = "/// Attached doc for refresh.\nfn refresh() {}\n\n// tiny\n\n// This standalone comment explains a subtle invariant worth keeping.\nfn validate() {}\n";
    let atoms = crate::ingest::code::parse_code(source, "src/auth.rs").unwrap();
    let units = build_units(&atoms, SourceKind::Code, "src/auth.rs");
    assert!(units
        .iter()
        .all(|unit| !unit.routing_text.contains("/// Attached")));
    let refresh = units
        .iter()
        .find(|unit| unit.routing_text.contains("> refresh"))
        .expect("refresh unit");
    assert!(refresh.evidence_text.contains("Attached doc for refresh."));
    assert!(units
        .iter()
        .all(|unit| !unit.evidence_text.contains("// tiny")));
    assert!(units
        .iter()
        .any(|unit| unit.evidence_text.contains("subtle invariant")));
}

#[test]
fn imports_aggregate_into_one_file_unit() {
    let source = "use std::collections::HashMap;\nuse std::fmt;\n\nfn refresh() {}\n";
    let atoms = crate::ingest::code::parse_code(source, "src/auth.rs").unwrap();
    let units = build_units(&atoms, SourceKind::Code, "src/auth.rs");
    let import_units: Vec<_> = units
        .iter()
        .filter(|unit| unit.metadata["unit_shape"] == "imports")
        .collect();
    assert_eq!(import_units.len(), 1);
    assert!(import_units[0]
        .evidence_text
        .contains("use std::collections::HashMap;"));
    assert!(import_units[0].evidence_text.contains("use std::fmt;"));
    assert_eq!(
        units
            .iter()
            .filter(|unit| unit
                .evidence_text
                .contains("use std::collections::HashMap;"))
            .count(),
        1
    );
}

/// Merged prose units keep one anchor per (kind, value, relationship); a
/// value shared by two anchor kinds must not collapse into one row.
#[test]
fn merged_prose_anchors_dedupe_on_kind_value_and_relationship() {
    // The same relative path appears as both a file mention and a symbol-ish
    // token, so a value-only dedupe would drop one of the two anchors.
    let text = "src/auth.rs handles `login`.\n\nThe `login` flow is documented.\n\nAlso see src/auth.rs specs.";
    let parsed = parse_markdown(&format!("# Auth\n\n{text}"), "README");
    let units = build_units(&parsed.atoms, SourceKind::Markdown, "README.md");
    for unit in &units {
        let mut seen = Vec::new();
        for anchor in &unit.anchors {
            assert!(
                !seen.contains(&(
                    anchor.kind,
                    anchor.value.clone(),
                    anchor.relationship.clone()
                )),
                "duplicate anchor {anchor:?} in {:?}",
                unit.anchors
            );
            seen.push((
                anchor.kind,
                anchor.value.clone(),
                anchor.relationship.clone(),
            ));
        }
    }
}
