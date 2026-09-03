use std::collections::BTreeSet;

use crate::core::{
    hash_segments, AnchorKind, AtomKind, BuiltAnchor, BuiltUnit, ParsedAtom, SourceKind, UnitKind,
};
use crate::metadata::source_slices::SourceSlice;

mod code;
mod prose;
#[cfg(test)]
mod tests;

pub const UNIT_BUILDER_VERSION: &str = "repo-unit-v2";
pub const MERGE_BELOW: usize = 120;
pub const MAX_TOKENS: usize = 800;

pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4).max(1)
}

fn content_atom(atom: &ParsedAtom) -> bool {
    !atom.text.trim().is_empty()
        && atom.text.chars().any(char::is_alphanumeric)
        && matches!(
            atom.kind,
            AtomKind::Paragraph
                | AtomKind::ListItem
                | AtomKind::BlockQuote
                | AtomKind::CodeBlock
                | AtomKind::Function
                | AtomKind::Class
                | AtomKind::Module
                | AtomKind::Declaration
                | AtomKind::Comment
        )
}

fn content_atom_at(atoms: &[ParsedAtom], index: usize) -> bool {
    if !content_atom(&atoms[index]) {
        return false;
    }
    if atoms[index].kind != AtomKind::Paragraph {
        return true;
    }
    let mut parent = atoms[index].parent_index;
    while let Some(parent_index) = parent {
        if matches!(
            atoms[parent_index].kind,
            AtomKind::ListItem | AtomKind::BlockQuote
        ) {
            return false;
        }
        parent = atoms[parent_index].parent_index;
    }
    true
}

pub(super) fn prepend_leading_context(evidence: &mut String, atom: &ParsedAtom) {
    if let Some(context) = crate::metadata::leading_context::read(&atom.metadata) {
        if !context.text.trim().is_empty() {
            evidence.push_str(context.text.trim_end());
            evidence.push_str("\n\n");
        }
    }
}

pub(super) fn whole_atom_evidence(atom: &ParsedAtom) -> String {
    let mut evidence = String::new();
    prepend_leading_context(&mut evidence, atom);
    evidence.push_str(&atom.breadcrumb);
    evidence.push_str("\n\n");
    evidence.push_str(&atom.text);
    evidence
}

fn unit_hash(kind: UnitKind, atom_hashes: &[&str], evidence: &str, routing: &str) -> String {
    let mut pieces = vec![UNIT_BUILDER_VERSION, kind.as_str()];
    pieces.extend_from_slice(atom_hashes);
    pieces.push(evidence);
    pieces.push(routing);
    hash_segments(&pieces)
}

fn mentions(text: &str) -> Vec<String> {
    fn insert(output: &mut BTreeSet<String>, value: &str) {
        let clean = value.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '/' | '_' | ':' | '.' | '-')
        });
        if !clean.is_empty() && clean.len() <= 256 {
            output.insert(clean.to_string());
        }
    }

    let mut output = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else { break };
        let value = &rest[..end];
        if !value.chars().any(char::is_whitespace) {
            insert(&mut output, value);
        }
        rest = &rest[end + 1..];
    }

    rest = text;
    while let Some(start) = rest.find("](") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find(')') else { break };
        insert(&mut output, &rest[..end]);
        rest = &rest[end + 1..];
    }

    for token in text.split_whitespace() {
        if token.contains("](") {
            continue;
        }
        let clean = token.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '/' | '_' | ':' | '.' | '-')
        });
        if clean.contains('/')
            || clean.contains("::")
            || clean.contains('_')
            || clean.ends_with(".rs")
            || clean.ends_with(".md")
        {
            insert(&mut output, clean);
        }
    }
    output.into_iter().take(32).collect()
}

fn file_anchor(locator: &str, relationship: &str) -> BuiltAnchor {
    BuiltAnchor {
        kind: AnchorKind::File,
        value: locator.to_string(),
        relationship: relationship.to_string(),
    }
}

fn code_anchors(locator: &str, atom: &ParsedAtom) -> Vec<BuiltAnchor> {
    let mut anchors = vec![file_anchor(locator, "defined_in")];
    if let Some(symbol) = crate::metadata::code_symbol::read(&atom.metadata)
        .map(|record| record.display_name)
        .filter(|symbol| !symbol.is_empty())
    {
        anchors.push(BuiltAnchor {
            kind: AnchorKind::Symbol,
            value: symbol,
            relationship: "defines".to_string(),
        });
    }
    anchors
}

fn prose_anchors(locator: &str, evidence: &str) -> Vec<BuiltAnchor> {
    let mut anchors = vec![file_anchor(locator, "documented_in")];
    for mention in mentions(evidence).into_iter().take(16) {
        if mention.contains('/') || mention.contains('.') {
            anchors.push(BuiltAnchor {
                kind: AnchorKind::File,
                value: mention,
                relationship: "mentions".to_string(),
            });
        } else {
            anchors.push(BuiltAnchor {
                kind: AnchorKind::Symbol,
                value: mention,
                relationship: "mentions".to_string(),
            });
        }
    }
    anchors
}

fn prose_routing(locator: &str, breadcrumb: &str, evidence: &str) -> String {
    let found = mentions(evidence);
    format!(
        "source: document\npath: {locator}\nheading: {breadcrumb}\nmentions: {}",
        found.join(" ")
    )
}

fn code_routing(locator: &str, atom: &ParsedAtom) -> String {
    let symbol = &atom.breadcrumb;
    let record = crate::metadata::code_symbol::read(&atom.metadata);
    let (signature, references) = match record {
        Some(record) => (record.signature, record.references.join(" ")),
        None => (String::new(), String::new()),
    };
    format!(
        "source: code\npath: {locator}\nsymbol: {symbol}\nkind: {}\nsignature: {signature}\nreferences: {references}",
        atom.kind.as_str()
    )
}

fn make_unit(
    kind: UnitKind,
    atom_indices: &[usize],
    atoms: &[ParsedAtom],
    evidence: String,
    routing: String,
    anchors: Vec<BuiltAnchor>,
) -> BuiltUnit {
    let hashes: Vec<&str> = atom_indices
        .iter()
        .map(|index| atoms[*index].content_hash.as_str())
        .collect();
    let source_slices: Vec<SourceSlice> = atom_indices
        .iter()
        .map(|index| SourceSlice {
            atom_hash: atoms[*index].content_hash.clone(),
            start_offset: atoms[*index].start_offset,
            end_offset: atoms[*index].end_offset,
            boundary: None,
        })
        .collect();
    let mut metadata = serde_json::json!({});
    crate::metadata::source_slices::set(&mut metadata, source_slices);
    BuiltUnit {
        kind,
        token_count: estimate_tokens(&evidence),
        content_hash: unit_hash(kind, &hashes, &evidence, &routing),
        evidence_text: evidence,
        routing_text: routing,
        metadata,
        anchors,
    }
}

/// Splits an episode body only when breadcrumb plus body exceeds the unit
/// token budget; otherwise the episode stays a single piece. Shared by the
/// Pi and Muse session families so every episode piece honors `MAX_TOKENS`.
pub fn split_episode_pieces(breadcrumb: &str, body: &str) -> Vec<String> {
    if estimate_tokens(&format!("{breadcrumb}\n\n{body}")) <= MAX_TOKENS {
        return vec![body.to_string()];
    }
    // Reserve room for the per-piece breadcrumb suffix so every piece
    // stays within the token budget.
    let max_chars = (MAX_TOKENS * 4)
        .saturating_sub(breadcrumb.chars().count() + 18)
        .max(1);
    split_oversized(body, max_chars)
        .into_iter()
        .map(|(piece, _, _)| piece)
        .collect()
}

pub fn split_oversized(text: &str, max_chars: usize) -> Vec<(String, usize, usize)> {
    let max_chars = max_chars.max(1);
    let mut output = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let candidate_end = text[start..]
            .char_indices()
            .nth(max_chars)
            .map(|(offset, _)| start + offset)
            .unwrap_or(text.len());
        let mut end = candidate_end;
        if end < text.len() {
            if let Some((offset, character)) = text[start..end]
                .char_indices()
                .rev()
                .find(|(_, character)| matches!(character, '.' | '\n'))
            {
                end = start + offset + character.len_utf8();
            }
        }
        if end <= start {
            end = candidate_end.max(start + 1).min(text.len());
        }
        output.push((text[start..end].to_string(), start, end));
        start = end;
    }
    output
}

pub fn build_units(atoms: &[ParsedAtom], source_kind: SourceKind, locator: &str) -> Vec<BuiltUnit> {
    match source_kind {
        SourceKind::Code => code::build_code(atoms, locator),
        SourceKind::Markdown | SourceKind::Text => prose::build_prose(atoms, locator),
        SourceKind::GitCommit | SourceKind::AgentSession => Vec::new(),
    }
}
