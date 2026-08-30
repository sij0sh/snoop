use std::collections::BTreeSet;
use std::ops::{Range, RangeInclusive};

use tree_sitter::{Node, Parser};

use crate::core::{AtomKind, ParsedAtom};

pub const CODE_EXTENSIONS: &[&str] = &["rs", "py", "pyi", "pyw", "ts", "tsx", "mts", "cts"];

struct Language {
    name: &'static str,
    language: fn() -> tree_sitter::Language,
    interesting: fn(&str) -> Option<AtomKind>,
    symbol_name: fn(Node<'_>, &str) -> Option<String>,
    leading_context: fn(Node<'_>, &str) -> Option<Range<usize>>,
    is_atomic: fn(Node<'_>) -> bool,
}

pub fn code_extension(path: &str) -> Option<&'static str> {
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    CODE_EXTENSIONS
        .iter()
        .copied()
        .find(|candidate| *candidate == extension)
}

pub fn supports_code_path(path: &str) -> bool {
    code_extension(path).is_some()
}

pub fn language_name(path: &str) -> Option<&'static str> {
    language_for(path).map(|language| language.name)
}

fn rust_interesting(kind: &str) -> Option<AtomKind> {
    match kind {
        "function_item" | "function_signature_item" => Some(AtomKind::Function),
        "struct_item" | "enum_item" | "union_item" => Some(AtomKind::Class),
        "trait_item" | "impl_item" | "mod_item" | "foreign_mod_item" | "type_item" => {
            Some(AtomKind::Module)
        }
        "const_item"
        | "static_item"
        | "use_declaration"
        | "extern_crate_declaration"
        | "macro_definition"
        | "macro_invocation" => Some(AtomKind::Declaration),
        "line_comment" | "block_comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

fn python_interesting(kind: &str) -> Option<AtomKind> {
    match kind {
        "function_definition" => Some(AtomKind::Function),
        "class_definition" => Some(AtomKind::Class),
        "type_alias_statement" | "import_statement" => Some(AtomKind::Declaration),
        "comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

fn typescript_interesting(kind: &str) -> Option<AtomKind> {
    match kind {
        "function_declaration"
        | "generator_function_declaration"
        | "function_signature"
        | "method_definition"
        | "method_signature" => Some(AtomKind::Function),
        "class_declaration" | "interface_declaration" => Some(AtomKind::Class),
        "type_alias_declaration" | "enum_declaration" | "import_statement" => {
            Some(AtomKind::Declaration)
        }
        "comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

fn rust_symbol_name(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() == "impl_item" {
        let target = node
            .child_by_field_name("type")
            .map(|child| source[child.byte_range()].trim())?;
        return Some(match node.child_by_field_name("trait") {
            Some(trait_node) => format!(
                "impl {} for {target}",
                source[trait_node.byte_range()].trim()
            ),
            None => format!("impl {target}"),
        });
    }
    field_symbol_name(node, source)
}

fn field_symbol_name(node: Node<'_>, source: &str) -> Option<String> {
    for field in ["name", "type"] {
        if let Some(name) = node.child_by_field_name(field) {
            let value = source[name.byte_range()].trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn sibling_context(node: Node<'_>, source: &str, kinds: &[&str]) -> Option<Range<usize>> {
    let mut left = node;
    let mut start = node.start_byte();
    while let Some(previous) = left.prev_named_sibling() {
        if !kinds.contains(&previous.kind()) {
            break;
        }
        let gap = source.get(previous.end_byte()..start)?;
        if gap.contains("\n\n") {
            break;
        }
        start = previous.start_byte();
        left = previous;
    }
    if start == node.start_byte() {
        None
    } else {
        Some(start..node.start_byte())
    }
}

fn rust_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    let range = sibling_context(node, source, &["line_comment", "block_comment"])?;
    let text = source.get(range.clone())?;
    let last_line = text.lines().next_back().unwrap_or_default().trim_start();
    (last_line.starts_with("///") || last_line.starts_with("//!")).then_some(range)
}

fn python_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    if let Some(parent) = node.parent() {
        if parent.kind() == "decorated_definition" {
            let mut start = node.start_byte();
            let mut cursor = parent.walk();
            for child in parent.named_children(&mut cursor) {
                if child.kind() == "decorator" && child.end_byte() <= node.start_byte() {
                    start = start.min(child.start_byte());
                }
            }
            if start != node.start_byte() {
                return Some(start..node.start_byte());
            }
            return None;
        }
    }
    sibling_context(node, source, &["comment"])
}

fn typescript_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["comment"])
}

fn rust_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string_literal" | "raw_string_literal" | "macro_definition"
    )
}

fn python_atomic(node: Node<'_>) -> bool {
    matches!(node.kind(), "string" | "concatenated_string")
}

fn typescript_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string" | "template_string" | "regex" | "jsx_element" | "jsx_fragment"
    )
}

fn language_for(locator: &str) -> Option<Language> {
    match code_extension(locator)? {
        "rs" => Some(Language {
            name: "rust",
            language: || tree_sitter_rust::LANGUAGE.into(),
            interesting: rust_interesting,
            symbol_name: rust_symbol_name,
            leading_context: rust_leading_context,
            is_atomic: rust_atomic,
        }),
        "py" | "pyi" | "pyw" => Some(Language {
            name: "python",
            language: || tree_sitter_python::LANGUAGE.into(),
            interesting: python_interesting,
            symbol_name: field_symbol_name,
            leading_context: python_leading_context,
            is_atomic: python_atomic,
        }),
        "ts" | "mts" | "cts" => Some(Language {
            name: "typescript",
            language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            interesting: typescript_interesting,
            symbol_name: field_symbol_name,
            leading_context: typescript_leading_context,
            is_atomic: typescript_atomic,
        }),
        "tsx" => Some(Language {
            name: "typescript",
            language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
            interesting: typescript_interesting,
            symbol_name: field_symbol_name,
            leading_context: typescript_leading_context,
            is_atomic: typescript_atomic,
        }),
        _ => None,
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
        if let Some(kind) = (self.language.interesting)(node.kind()) {
            let name = (self.language.symbol_name)(node, source).unwrap_or_else(|| {
                source[node.byte_range()]
                    .lines()
                    .next()
                    .unwrap_or(node.kind())
                    .trim()
                    .chars()
                    .take(80)
                    .collect()
            });
            let parent = self.enclosing.last().copied().unwrap_or(self.file_index);
            let breadcrumb = format!("{} > {}", self.atoms[parent].breadcrumb, name);
            let text = source[node.byte_range()].to_string();
            let mut references = BTreeSet::new();
            collect_identifiers(node, source, &mut references);
            references.remove(&name);
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
                    "symbol": name,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_symbols_include_signature_and_references() {
        let atoms = parse_code(
            "pub fn refresh_session(token: Token) { validate_token(token); }",
            "src/auth.rs",
        )
        .unwrap();
        let function = atoms
            .iter()
            .find(|atom| atom.kind == AtomKind::Function)
            .unwrap();
        assert_eq!(function.breadcrumb, "src/auth.rs > refresh_session");
        assert!(function.metadata["signature"]
            .as_str()
            .unwrap()
            .contains("refresh_session"));
        assert!(function.metadata["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "validate_token"));
    }

    #[test]
    fn trait_signatures_and_trait_impls_have_distinct_qualified_names() {
        let source = r#"
trait Build { fn create() -> Self; }
struct Item;
impl Display for Item { fn fmt(&self) {} }
impl Debug for Item { fn fmt(&self) {} }
"#;
        let atoms = parse_code(source, "src/lib.rs").unwrap();
        assert!(atoms.iter().any(|atom| {
            atom.kind == AtomKind::Function && atom.breadcrumb.ends_with("Build > create")
        }));
        assert!(atoms
            .iter()
            .any(|atom| atom.breadcrumb.contains("impl Display for Item > fmt")));
        assert!(atoms
            .iter()
            .any(|atom| atom.breadcrumb.contains("impl Debug for Item > fmt")));
    }

    #[test]
    fn python_symbols_map_to_atom_kinds() {
        let source = "\"\"\"Docs.\"\"\"\nimport json\n\nclass TokenStore:\n    def refresh(self, token):\n        validate(token)\n\ndef load_config(path):\n    return json.load(open(path))\n";
        let atoms = parse_code(source, "src/store.py").unwrap();
        assert_eq!(atoms[0].metadata["language"], "python");
        let class = atoms
            .iter()
            .find(|atom| atom.kind == AtomKind::Class)
            .unwrap();
        assert!(class.breadcrumb.ends_with("TokenStore"));
        let function = atoms
            .iter()
            .find(|atom| atom.kind == AtomKind::Function && atom.breadcrumb.ends_with("refresh"))
            .expect("method refresh becomes a Function atom");
        assert!(function.breadcrumb.contains("TokenStore > refresh"));
        assert!(function.metadata["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "validate"));
    }

    #[test]
    fn typescript_symbols_map_to_atom_kinds() {
        let source = "import fs from 'fs';\n\ninterface Session { id: string }\n\ntype Token = string;\n\nexport class Auth {\n  refresh(id: string): void {\n    validate(id);\n  }\n}\n\nfunction load(id: string): Token {\n  return id;\n}\n";
        let atoms = parse_code(source, "src/auth.ts").unwrap();
        assert_eq!(atoms[0].metadata["language"], "typescript");
        for needle in ["Auth > refresh", "Session", "Token", "load"] {
            assert!(
                atoms.iter().any(|atom| atom.breadcrumb.ends_with(needle)),
                "missing breadcrumb ending in {needle}"
            );
        }
    }

    #[test]
    fn nested_symbols_are_emitted_exactly_once() {
        let rust_source = "impl Store {\n    fn refresh(&self) {}\n    fn validate(&self) {}\n}\n";
        let rust_atoms = parse_code(rust_source, "src/store.rs").unwrap();
        let rust_functions: Vec<_> = rust_atoms
            .iter()
            .filter(|atom| atom.kind == AtomKind::Function)
            .collect();
        assert_eq!(
            rust_functions.len(),
            2,
            "methods inside an impl block must not be duplicated: {:?}",
            rust_functions
                .iter()
                .map(|atom| &atom.breadcrumb)
                .collect::<Vec<_>>()
        );

        let python_source =
            "class TokenStore:\n    def refresh(self, token):\n        validate(token)\n";
        let python_atoms = parse_code(python_source, "src/store.py").unwrap();
        let python_functions: Vec<_> = python_atoms
            .iter()
            .filter(|atom| atom.kind == AtomKind::Function)
            .collect();
        assert_eq!(
            python_functions.len(),
            1,
            "methods inside a class must not be duplicated"
        );
    }

    #[test]
    fn tsx_files_parse_with_jsx_extension() {
        let tsx = "export function App() {\n  return <div className='x'>hi</div>;\n}\n";
        let atoms = parse_code(tsx, "src/app.tsx").unwrap();
        assert_eq!(atoms[0].metadata["language"], "typescript");
        assert!(atoms.iter().any(|atom| atom.breadcrumb.ends_with("App")));
    }

    #[test]
    fn code_extensions_cover_the_shared_registry() {
        for locator in [
            "a.rs", "a.py", "a.pyi", "a.pyw", "a.ts", "a.tsx", "a.mts", "a.cts",
        ] {
            assert!(supports_code_path(locator), "{locator} must be supported");
        }
        for locator in [
            "a.md",
            "a.txt",
            "README",
            "a.json",
            ".gitignore",
            "a.js",
            "a.jsx",
            "a.mjs",
            "a.cjs",
            "a.go",
            "a.java",
            "a.c",
            "a.cs",
            "a.php",
            "a.sh",
            "a.tf",
        ] {
            assert!(!supports_code_path(locator), "{locator} must not be code");
        }
        assert_eq!(language_name("src/app.tsx"), Some("typescript"));
        assert_eq!(language_name("src/store.pyi"), Some("python"));
    }

    #[test]
    fn analyze_code_projects_qualified_boundaries() {
        let source = "class TokenStore:\n    def refresh(self, token):\n        validate(token)\n";
        let boundaries = analyze_code("src/store.py", source).unwrap();
        let refresh = boundaries
            .iter()
            .find(|boundary| boundary.display_name == "refresh")
            .unwrap();
        assert_eq!(refresh.symbol_id, "src/store.py > TokenStore > refresh");
        assert_eq!(refresh.qualified_name, "TokenStore > refresh");
        assert_eq!(
            refresh.parent_symbol_id.as_deref(),
            Some("src/store.py > TokenStore")
        );
        assert_eq!(refresh.language, "python");
        assert_eq!(refresh.kind, AtomKind::Function);
        assert!(refresh
            .references
            .iter()
            .any(|reference| reference == "validate"));
        assert_eq!(*refresh.line_range.start(), 2);
        assert_eq!(*refresh.line_range.end(), 3);
        assert_eq!(
            refresh.byte_range.start,
            source.find("def refresh").unwrap()
        );
    }

    #[test]
    fn leading_context_attaches_to_declarations() {
        let rust = "/// Refresh the token.\nfn refresh() {}\n";
        let rust_boundaries = analyze_code("src/auth.rs", rust).unwrap();
        let rust_refresh = rust_boundaries
            .iter()
            .find(|boundary| boundary.display_name == "refresh")
            .unwrap();
        assert_eq!(
            &rust[rust_refresh.leading_context.clone().unwrap()],
            "/// Refresh the token.\n"
        );

        let python = "@cached\nclass Store:\n    pass\n";
        let python_boundaries = analyze_code("src/store.py", python).unwrap();
        let store = python_boundaries
            .iter()
            .find(|boundary| boundary.display_name == "Store")
            .unwrap();
        assert_eq!(&python[store.leading_context.clone().unwrap()], "@cached\n");

        let typescript = "/** Loads config. */\nfunction load() {}\n";
        let ts_boundaries = analyze_code("src/load.ts", typescript).unwrap();
        let load = ts_boundaries
            .iter()
            .find(|boundary| boundary.display_name == "load")
            .unwrap();
        assert_eq!(
            &typescript[load.leading_context.clone().unwrap()],
            "/** Loads config. */\n"
        );
    }

    #[test]
    fn oversized_splitting_avoids_atomic_interiors() {
        let long = "x".repeat(2_000);
        let filler = "y".repeat(2_000);
        let source = format!(
            "fn big() {{\n    let first = \"{long}\";\n    let second = \"{filler}\";\n}}\n"
        );
        let atoms = parse_code(&source, "src/big.rs").unwrap();
        let function = atoms
            .iter()
            .find(|atom| atom.kind == AtomKind::Function)
            .unwrap();
        let segments = function.metadata["chunk_segments"].as_array().unwrap();
        assert!(!segments.is_empty(), "the function must be segmented");
        let first_start = source.find(&long).unwrap();
        let first_end = first_start + long.len();
        let second_start = source.rfind(&filler).unwrap();
        let second_end = second_start + filler.len();
        for segment in segments {
            let start = segment["start_offset"].as_u64().unwrap() as usize;
            let end = segment["end_offset"].as_u64().unwrap() as usize;
            let cuts_first = start < first_end && end > first_start;
            let cuts_second = start < second_end && end > second_start;
            if cuts_first {
                assert!(
                    start <= first_start && end >= first_end,
                    "segment {start}..{end} cuts the first string"
                );
            }
            if cuts_second {
                assert!(
                    start <= second_start && end >= second_end,
                    "segment {start}..{end} cuts the second string"
                );
            }
        }
    }
}
