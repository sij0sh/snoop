use std::collections::BTreeSet;
use std::ops::{Range, RangeInclusive};

use tree_sitter::{Node, Parser};
use std::cell::Cell;

// Scaling-guard counter (computational-scaling-audit 20260830195149-6f1a96a5,
// finding 2). Invariant: bytes scanned per analyze_code <= 2 x S_f for flat
// symbol layout (was Theta(A' x S_f) prefix rescans).
//
// Thread-local so concurrent analyze_code calls (test parallelism) do not
// pool their bytes into one shared total; each thread only ever measures its
// own analyze_code run (defect-audit 20260831023057-8ecdc8ca c1).
thread_local! {
    pub(crate) static LINE_SCAN_BYTES: Cell<u64> = const { Cell::new(0) };
}

/// Read and reset the scaling-guard counter for the current thread.
/// Callers take the baseline before and the total after an analyze_code run.
#[cfg(test)]
pub(crate) fn take_line_scan_bytes() -> u64 {
    LINE_SCAN_BYTES.with(|counter| {
        let total = counter.get();
        counter.set(0);
        total
    })
}

use crate::core::{AtomKind, ParsedAtom};
use crate::metadata::chunk_segments::ChunkSegment;

mod languages;
#[cfg(test)]
mod tests;

pub use languages::{code_extension, language_name, supports_code_path};
use languages::{language_for_source, Language, SymbolInfo};

pub fn parse_code(source: &str, locator: &str) -> Result<Vec<ParsedAtom>, String> {
    let language = language_for_source(locator, source)
        .ok_or_else(|| format!("unsupported code locator: {locator}"))?;
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

        // Script languages execute meaningful code at the top level, so
        // the root program becomes a Module overview unit.
        let root_overview = node.parent().is_none() && self.language.root_overview;
        let kind = if root_overview {
            Some(AtomKind::Module)
        } else {
            (self.language.interesting)(node, source)
        };
        if let Some(kind) = kind {
            let info = if root_overview {
                SymbolInfo::plain("program".to_string())
            } else {
                (self.language.symbol_info)(node, source).unwrap_or_else(|| {
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
                })
            };
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
            let is_import = !root_overview && (self.language.is_import)(node, source);
            let mut metadata = serde_json::json!({
                "file": self.locator,
                "chunk_alternatives": crate::metadata::chunk_segments::value(&alternative_segments),
                "node_kind": node.kind(),
            });
            crate::metadata::code_symbol::write(
                &mut metadata,
                &info.display_name,
                &signature(node, source),
                &references,
            );
            crate::metadata::chunk_segments::set(&mut metadata, segments);
            crate::metadata::is_import::set(&mut metadata, is_import);
            crate::metadata::leading_context::set(
                &mut metadata,
                leading_context.map(|range| crate::metadata::leading_context::LeadingContext {
                    start_offset: range.start,
                    end_offset: range.end,
                    text: source.get(range).unwrap_or_default().to_string(),
                }),
            );
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
                metadata,
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
) -> Vec<ChunkSegment> {
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
            segments.push(ChunkSegment {
                start_offset: start,
                end_offset: last_legal,
                boundary: "ast".to_string(),
            });
            start = last_legal;
        }
        while source[start..boundary].chars().count() > max_chars {
            let end = byte_after_chars(source, start, boundary, max_chars);
            segments.push(ChunkSegment {
                start_offset: start,
                end_offset: end,
                boundary: "lexical_fallback".to_string(),
            });
            start = end;
        }
        last_legal = boundary;
    }
    if start < node.end_byte() {
        segments.push(ChunkSegment {
            start_offset: start,
            end_offset: node.end_byte(),
            boundary: if last_legal == node.end_byte() {
                "ast"
            } else {
                "lexical_fallback"
            }
            .to_string(),
        });
    }
    segments
}

/// Monotone (byte, line) cursor (audit finding 2): one forward pass per
/// analyze_code computes every symbol boundary's line instead of recounting
/// newlines from byte 0 per boundary. Boundary bytes arrive in ascending
/// order (tree-sitter pre-order); a non-ascending boundary (nested atom
/// queried after its parent's end) restarts the scan from byte 0.
struct LineCursor<'a> {
    source: &'a [u8],
    byte: usize,
    line: u32,
}

impl<'a> LineCursor<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            byte: 0,
            line: 1,
        }
    }

    fn line_of(&mut self, byte: usize) -> u32 {
        let capped = byte.min(self.source.len());
        if capped < self.byte {
            self.byte = 0;
            self.line = 1;
        }
        let newlines = self.source[self.byte..capped]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count() as u32;
        LINE_SCAN_BYTES.with(|scanned| {
            scanned.set(scanned.get() + (capped - self.byte) as u64);
        });
        self.line += newlines;
        self.byte = capped;
        self.line
    }
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
    let mut cursor = LineCursor::new(source);
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
            let leading_context = crate::metadata::leading_context::read(&atom.metadata)
                .map(|context| context.start_offset..context.end_offset);
            let record = crate::metadata::code_symbol::read(&atom.metadata);
            CodeBoundary {
                language: language.clone(),
                kind: atom.kind,
                symbol_id: atom.breadcrumb.clone(),
                display_name: record
                    .as_ref()
                    .map(|record| record.display_name.clone())
                    .unwrap_or_default(),
                qualified_name,
                signature: record
                    .as_ref()
                    .map(|record| record.signature.clone())
                    .filter(|signature| !signature.is_empty()),
                parent_symbol_id,
                byte_range: atom.start_offset..atom.end_offset,
                line_range: cursor.line_of(atom.start_offset)
                    ..=cursor.line_of(
                        atom.end_offset.saturating_sub(1).max(atom.start_offset),
                    ),
                leading_context,
                references: record.map(|record| record.references).unwrap_or_default(),
                safe_split_points: crate::metadata::chunk_segments::read(&atom.metadata)
                    .iter()
                    .map(|segment| segment.end_offset)
                    .collect(),
            }
        })
        .collect()
}
