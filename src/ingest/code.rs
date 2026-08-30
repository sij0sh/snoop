use std::collections::BTreeSet;
use std::ops::{Range, RangeInclusive};

use tree_sitter::{Node, Parser};

use crate::core::{AtomKind, ParsedAtom};

mod languages;
#[cfg(test)]
mod tests;

pub use languages::{code_extension, language_name, supports_code_path};
use languages::{language_for, Language, SymbolInfo};

pub fn parse_code(source: &str, locator: &str) -> Result<Vec<ParsedAtom>, String> {
    let language =
        language_for(locator).ok_or_else(|| format!("unsupported code locator: {locator}"))?;
    let mut parser = Parser::new();
    parser
        .set_language(&(language.language)())
        .map_err(|error| error.to_string())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "tree-sitter returned no syntax tree".to_string())?;

    let atoms = vec![ParsedAtom {
        kind: AtomKind::File,
        parent_index: None,
        ordinal: 0,
        start_offset: 0,
        end_offset: source.len(),
        text: source.to_string(),
        breadcrumb: locator.to_string(),
        content_hash: ParsedAtom::content_hash_of(AtomKind::File, locator, source),
        metadata: serde_json::json!({"file": locator, "language": language.name}),
    }];
    let mut context = EmitContext {
        source,
        locator,
        file_index: 0,
        enclosing: Vec::new(),
        ordinal: 1,
        atoms,
        language: &language,
    };
    context.walk(tree.root_node());
    Ok(context.atoms)
}

struct EmitContext<'a> {
    source: &'a str,
    locator: &'a str,
    file_index: usize,
    enclosing: Vec<usize>,
    ordinal: u32,
    atoms: Vec<ParsedAtom>,
    language: &'a Language,
}

impl<'a> EmitContext<'a> {
    fn walk(&mut self, node: Node<'_>) {
        let pushed = self.emit(node);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.walk(child);
        }
        if pushed {
            self.enclosing.pop();
        }
    }

    fn emit(&mut self, node: Node<'_>) -> bool {
        let source = self.source;
        if let Some(kind) = (self.language.interesting)(node, source) {
            let info = (self.language.symbol_info)(node, source).unwrap_or_else(|| {
                SymbolInfo::plain(
                    source[node.byte_range()]
                        .lines()
                        .next()
                        .unwrap_or(node.kind())
                        .trim()
                        .chars()
                        .take(80)
                        .collect(),
                )
            });
            let parent = self.enclosing.last().copied().unwrap_or(self.file_index);
            let breadcrumb = format!(
                "{} > {}",
                self.atoms[parent].breadcrumb, info.qualified_component
            );
            let text = source[node.byte_range()].to_string();
            let mut references = BTreeSet::new();
            collect_identifiers(node, source, &mut references);
            references.remove(&info.display_name);
            let references: Vec<String> = references.into_iter().take(64).collect();
            let max_chars = (crate::ingest::units::MAX_TOKENS * 4)
                .saturating_sub(breadcrumb.chars().count() + 2)
                .max(1);
            let segments = legal_segments(node, source, max_chars, self.language.is_atomic);
            let alternative_segments = if segments.is_empty() {
                Vec::new()
            } else {
                legal_segments(
                    node,
                    source,
                    (max_chars * 3 / 4).max(1),
                    self.language.is_atomic,
                )
            };
            let leading_context = (self.language.leading_context)(node, source);
            let index = self.atoms.len();
            self.atoms.push(ParsedAtom {
                kind,
                parent_index: Some(parent),
                ordinal: self.ordinal,
                start_offset: node.start_byte(),
                end_offset: node.end_byte(),
                content_hash: ParsedAtom::content_hash_of(kind, &breadcrumb, &text),
                text,
                breadcrumb: breadcrumb.clone(),
                metadata: serde_json::json!({
                    "file": self.locator,
                    "symbol": info.display_name,
                    "signature": signature(node, source),
                    "references": references,
                    "chunk_segments": segments,
                    "chunk_alternatives": alternative_segments,
                    "node_kind": node.kind(),
                    "leading_context": leading_context.map(|range| serde_json::json!({
                        "start_offset": range.start,
                        "end_offset": range.end,
                        "text": source.get(range).unwrap_or_default(),
                    })),
                }),
            });
            self.ordinal += 1;
            self.enclosing.push(index);
            true
        } else {
            false
        }
    }
}

fn signature(node: Node<'_>, source: &str) -> String {
    if let Some(body) = node.child_by_field_name("body") {
        let value = source[node.start_byte()..body.start_byte()].trim();
        if !value.is_empty() {
            return value.to_string();
        }
    }
    source[node.byte_range()]
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn collect_identifiers(node: Node<'_>, source: &str, output: &mut BTreeSet<String>) {
    if node.kind().ends_with("identifier") {
        let value = source[node.byte_range()].trim();
        if !value.is_empty() && value.len() <= 100 {
            output.insert(value.to_string());
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_identifiers(child, source, output);
    }
}

fn legal_segments(
    node: Node<'_>,
    source: &str,
    max_chars: usize,
    is_atomic: fn(Node<'_>) -> bool,
) -> Vec<serde_json::Value> {
    fn collect_ends(node: Node<'_>, is_atomic: fn(Node<'_>) -> bool, ends: &mut Vec<usize>) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if is_atomic(child) {
                ends.push(child.start_byte());
                ends.push(child.end_byte());
            } else {
                ends.push(child.end_byte());
                collect_ends(child, is_atomic, ends);
            }
        }
    }

    fn byte_after_chars(text: &str, start: usize, end: usize, count: usize) -> usize {
        text[start..end]
            .char_indices()
            .nth(count)
            .map(|(offset, _)| start + offset)
            .unwrap_or(end)
    }

    if source[node.byte_range()].chars().count() <= max_chars {
        return Vec::new();
    }
    let mut ends = Vec::new();
    collect_ends(node, is_atomic, &mut ends);
    ends.push(node.end_byte());
    ends.sort_unstable();
    ends.dedup();

    let mut segments = Vec::new();
    let mut start = node.start_byte();
    let mut last_legal = start;
    for boundary in ends {
        if boundary <= start || boundary > node.end_byte() {
            continue;
        }
        if source[start..boundary].chars().count() <= max_chars {
            last_legal = boundary;
            continue;
        }
        if last_legal > start {
            segments.push(serde_json::json!({
                "start_offset": start,
                "end_offset": last_legal,
                "boundary": "ast",
            }));
            start = last_legal;
        }
        while source[start..boundary].chars().count() > max_chars {
            let end = byte_after_chars(source, start, boundary, max_chars);
            segments.push(serde_json::json!({
                "start_offset": start,
                "end_offset": end,
                "boundary": "lexical_fallback",
            }));
            start = end;
        }
        last_legal = boundary;
    }
    if start < node.end_byte() {
        segments.push(serde_json::json!({
            "start_offset": start,
            "end_offset": node.end_byte(),
            "boundary": if last_legal == node.end_byte() { "ast" } else { "lexical_fallback" },
        }));
    }
    segments
}

fn line_of(source: &str, byte: usize) -> u32 {
    let capped = byte.min(source.len());
    source.as_bytes()[..capped]
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count() as u32
        + 1
}

#[derive(Debug, Clone)]
pub struct CodeBoundary {
    pub language: String,
    pub kind: AtomKind,
    pub symbol_id: String,
    pub display_name: String,
    pub qualified_name: String,
    pub signature: Option<String>,
    pub parent_symbol_id: Option<String>,
    pub byte_range: Range<usize>,
    pub line_range: RangeInclusive<u32>,
    pub leading_context: Option<Range<usize>>,
    pub references: Vec<String>,
    pub safe_split_points: Vec<usize>,
}

pub fn analyze_code(path: &str, source: &str) -> Result<Vec<CodeBoundary>, String> {
    let atoms = parse_code(source, path)?;
    Ok(boundaries_from_atoms(path, source, &atoms))
}

fn boundaries_from_atoms(path: &str, source: &str, atoms: &[ParsedAtom]) -> Vec<CodeBoundary> {
    let language = atoms
        .first()
        .and_then(|atom| atom.metadata["language"].as_str())
        .unwrap_or("unknown")
        .to_string();
    let prefix = format!("{path} > ");
    atoms
        .iter()
        .filter(|atom| {
            matches!(
                atom.kind,
                AtomKind::Function | AtomKind::Class | AtomKind::Module | AtomKind::Declaration
            )
        })
        .map(|atom| {
            let qualified_name = atom
                .breadcrumb
                .strip_prefix(&prefix)
                .unwrap_or(&atom.breadcrumb)
                .to_string();
            let parent_symbol_id = atom
                .parent_index
                .and_then(|parent| atoms.get(parent))
                .filter(|parent| parent.kind != AtomKind::File)
                .map(|parent| parent.breadcrumb.clone());
            let leading_context = atom.metadata["leading_context"]["start_offset"]
                .as_u64()
                .zip(atom.metadata["leading_context"]["end_offset"].as_u64())
                .map(|(start, end)| start as usize..end as usize);
            CodeBoundary {
                language: language.clone(),
                kind: atom.kind,
                symbol_id: atom.breadcrumb.clone(),
                display_name: atom.metadata["symbol"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                qualified_name,
                signature: atom.metadata["signature"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(String::from),
                parent_symbol_id,
                byte_range: atom.start_offset..atom.end_offset,
                line_range: line_of(source, atom.start_offset)
                    ..=line_of(
                        source,
                        atom.end_offset.saturating_sub(1).max(atom.start_offset),
                    ),
                leading_context,
                references: atom.metadata["references"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                safe_split_points: atom.metadata["chunk_segments"]
                    .as_array()
                    .map(|segments| {
                        segments
                            .iter()
                            .filter_map(|segment| segment["end_offset"].as_u64())
                            .map(|value| value as usize)
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect()
}
