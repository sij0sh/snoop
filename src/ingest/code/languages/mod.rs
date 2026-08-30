//! Language registry: extensions, grammars, and per-language AST callbacks.

use std::ops::Range;

use tree_sitter::Node;

use crate::core::AtomKind;

mod csharp;

use csharp::{csharp_atomic, csharp_interesting, csharp_leading_context, csharp_symbol_info};

pub const CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "pyi", "pyw", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "go", "java",
    "cs",
];

pub struct Language {
    pub name: &'static str,
    pub language: fn() -> tree_sitter::Language,
    pub interesting: fn(Node<'_>, &str) -> Option<AtomKind>,
    pub symbol_info: fn(Node<'_>, &str) -> Option<SymbolInfo>,
    pub leading_context: fn(Node<'_>, &str) -> Option<Range<usize>>,
    pub is_atomic: fn(Node<'_>) -> bool,
}

/// One symbol identity: the rendered name and the qualified breadcrumb
/// component. Existing languages keep both equal; C++ splits them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolInfo {
    pub display_name: String,
    pub qualified_component: String,
}

impl SymbolInfo {
    pub fn plain(name: String) -> Self {
        Self {
            display_name: name.clone(),
            qualified_component: name,
        }
    }
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

fn rust_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
    match node.kind() {
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

fn python_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
    match node.kind() {
        "function_definition" => Some(AtomKind::Function),
        "class_definition" => Some(AtomKind::Class),
        "type_alias_statement" | "import_statement" => Some(AtomKind::Declaration),
        "comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

fn ecmascript_interesting(node: Node<'_>, source: &str) -> Option<AtomKind> {
    match node.kind() {
        "function_declaration"
        | "generator_function_declaration"
        | "function_signature"
        | "method_definition"
        | "method_signature" => Some(AtomKind::Function),
        "class_declaration" | "interface_declaration" => Some(AtomKind::Class),
        "type_alias_declaration" | "enum_declaration" | "import_statement" => {
            Some(AtomKind::Declaration)
        }
        "variable_declarator" => node
            .child_by_field_name("value")
            .is_some_and(|value| callable_kind(value))
            .then_some(AtomKind::Function),
        "assignment_expression" => export_assignment_kind(node, source),
        "comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

fn go_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
    match node.kind() {
        "function_declaration" | "method_declaration" => Some(AtomKind::Function),
        "type_spec" => {
            let aggregate = node
                .child_by_field_name("type")
                .is_some_and(|value| matches!(value.kind(), "struct_type" | "interface_type"));
            Some(if aggregate {
                AtomKind::Class
            } else {
                AtomKind::Declaration
            })
        }
        "type_alias" | "const_spec" | "var_spec" | "import_spec" => Some(AtomKind::Declaration),
        "comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

fn java_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
    match node.kind() {
        "class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration"
        | "annotation_type_declaration" => Some(AtomKind::Class),
        "method_declaration" | "constructor_declaration" | "compact_constructor_declaration" => {
            Some(AtomKind::Function)
        }
        "field_declaration" | "import_declaration" => Some(AtomKind::Declaration),
        "line_comment" | "block_comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

fn callable_kind(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "arrow_function" | "function_expression" | "generator_function" | "function"
    )
}

fn export_assignment_kind(node: Node<'_>, source: &str) -> Option<AtomKind> {
    let left = node.child_by_field_name("left")?;
    if left.kind() != "member_expression" || !targets_exports(left, source) {
        return None;
    }
    let callable = node
        .child_by_field_name("right")
        .is_some_and(|value| callable_kind(value));
    Some(if callable {
        AtomKind::Function
    } else {
        AtomKind::Declaration
    })
}

fn targets_exports(left: Node<'_>, source: &str) -> bool {
    let mut chain = Vec::new();
    let mut current = left;
    while current.kind() == "member_expression" {
        chain.push(current);
        current = match current.child_by_field_name("object") {
            Some(object) => object,
            None => return false,
        };
    }
    if current.kind() != "identifier" {
        return false;
    }
    let root = source[current.byte_range()].trim();
    let first_property = chain
        .last()
        .and_then(|member| member.child_by_field_name("property"))
        .map(|property| source[property.byte_range()].trim())
        .unwrap_or_default();
    match root {
        "exports" => true,
        "module" => first_property == "exports",
        _ => false,
    }
}

fn rust_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    if node.kind() == "impl_item" {
        let target = node
            .child_by_field_name("type")
            .map(|child| source[child.byte_range()].trim())?;
        return Some(SymbolInfo::plain(match node.child_by_field_name("trait") {
            Some(trait_node) => format!(
                "impl {} for {target}",
                source[trait_node.byte_range()].trim()
            ),
            None => format!("impl {target}"),
        }));
    }
    field_symbol_info(node, source)
}

fn field_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    first_field_text(node, source, &["name", "type"]).map(SymbolInfo::plain)
}

fn first_field_text(node: Node<'_>, source: &str, fields: &[&str]) -> Option<String> {
    for field in fields {
        if let Some(matched) = node.child_by_field_name(field) {
            let value = source[matched.byte_range()].trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn ecmascript_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    if node.kind() == "assignment_expression" {
        if let Some(left) = node.child_by_field_name("left") {
            let value = source[left.byte_range()].trim();
            if !value.is_empty() {
                return Some(SymbolInfo::plain(value.to_string()));
            }
        }
    }
    field_symbol_info(node, source)
}

fn go_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    match node.kind() {
        "method_declaration" => {
            let name = node.child_by_field_name("name")?;
            let receiver = node.child_by_field_name("receiver")?;
            let method = source[name.byte_range()].trim();
            let base = source[receiver.byte_range()]
                .split_whitespace()
                .next_back()?
                .trim_matches(|character: char| matches!(character, '(' | ')' | '*'))
                .trim();
            if base.is_empty() || method.is_empty() {
                return None;
            }
            Some(SymbolInfo::plain(format!("{base}.{method}")))
        }
        "import_spec" => first_field_text(node, source, &["name", "path"]).map(SymbolInfo::plain),
        _ => field_symbol_info(node, source),
    }
}

fn java_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    match node.kind() {
        "field_declaration" => field_names(node, source).map(SymbolInfo::plain),
        "import_declaration" => import_symbol_name(node, source).map(SymbolInfo::plain),
        _ => field_symbol_info(node, source),
    }
}

fn field_names(node: Node<'_>, source: &str) -> Option<String> {
    let mut names: Vec<&str> = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "variable_declarator" {
            continue;
        }
        if let Some(name) = child.child_by_field_name("name") {
            let value = source[name.byte_range()].trim();
            if !value.is_empty() {
                names.push(value);
            }
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

fn import_symbol_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let import = node
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"));
    import.map(|child| source[child.byte_range()].trim().to_string())
}

/// Walk a C-family declarator to its innermost identifier.
///
/// Recursion covers the declarator wrappers; qualified-component
/// composition for `qualified_identifier` lands with the C++ adapter.
/// A function pointer yields the variable identity so adapters can
/// classify it as a plain declaration.
// Consumed by the C and C++ adapters in later plan phases.
#[allow(dead_code)]
pub(super) fn declarator_name(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    match node.kind() {
        "identifier" | "type_identifier" => {
            let name = source[node.byte_range()].trim();
            (!name.is_empty()).then(|| SymbolInfo::plain(name.to_string()))
        }
        "function_declarator"
        | "pointer_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "attributed_declarator"
        | "init_declarator" => node
            .child_by_field_name("declarator")
            .or_else(|| node.named_child(0))
            .and_then(|child| declarator_name(child, source)),
        _ => None,
    }
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

fn ecmascript_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["comment"])
}

fn go_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["comment"])
}

fn java_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["line_comment", "block_comment"])
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

fn ecmascript_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string" | "template_string" | "regex" | "jsx_element" | "jsx_fragment"
    )
}

fn go_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "interpreted_string_literal" | "raw_string_literal" | "rune_literal"
    )
}

fn java_atomic(node: Node<'_>) -> bool {
    matches!(node.kind(), "string_literal" | "character_literal")
}

pub fn language_for(locator: &str) -> Option<Language> {
    match code_extension(locator)? {
        "rs" => Some(Language {
            name: "rust",
            language: || tree_sitter_rust::LANGUAGE.into(),
            interesting: rust_interesting,
            symbol_info: rust_symbol_info,
            leading_context: rust_leading_context,
            is_atomic: rust_atomic,
        }),
        "py" | "pyi" | "pyw" => Some(Language {
            name: "python",
            language: || tree_sitter_python::LANGUAGE.into(),
            interesting: python_interesting,
            symbol_info: field_symbol_info,
            leading_context: python_leading_context,
            is_atomic: python_atomic,
        }),
        "ts" | "mts" | "cts" => Some(Language {
            name: "typescript",
            language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            interesting: ecmascript_interesting,
            symbol_info: ecmascript_symbol_info,
            leading_context: ecmascript_leading_context,
            is_atomic: ecmascript_atomic,
        }),
        "tsx" => Some(Language {
            name: "typescript",
            language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
            interesting: ecmascript_interesting,
            symbol_info: ecmascript_symbol_info,
            leading_context: ecmascript_leading_context,
            is_atomic: ecmascript_atomic,
        }),
        "js" | "jsx" | "mjs" | "cjs" => Some(Language {
            name: "javascript",
            language: || tree_sitter_javascript::LANGUAGE.into(),
            interesting: ecmascript_interesting,
            symbol_info: ecmascript_symbol_info,
            leading_context: ecmascript_leading_context,
            is_atomic: ecmascript_atomic,
        }),
        "go" => Some(Language {
            name: "go",
            language: || tree_sitter_go::LANGUAGE.into(),
            interesting: go_interesting,
            symbol_info: go_symbol_info,
            leading_context: go_leading_context,
            is_atomic: go_atomic,
        }),
        "java" => Some(Language {
            name: "java",
            language: || tree_sitter_java::LANGUAGE.into(),
            interesting: java_interesting,
            symbol_info: java_symbol_info,
            leading_context: java_leading_context,
            is_atomic: java_atomic,
        }),
        "cs" => Some(Language {
            name: "csharp",
            language: || tree_sitter_c_sharp::LANGUAGE.into(),
            interesting: csharp_interesting,
            symbol_info: csharp_symbol_info,
            leading_context: csharp_leading_context,
            is_atomic: csharp_atomic,
        }),
        _ => None,
    }
}
