//! Java adapter: AST classification, symbols, context, and atomic ranges.

use std::ops::Range;

use tree_sitter::Node;

use super::{field_symbol_info, sibling_context, SymbolInfo};
use crate::core::AtomKind;

pub(super) fn java_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
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

pub(super) fn java_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
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

pub(super) fn java_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["line_comment", "block_comment"])
}

pub(super) fn java_atomic(node: Node<'_>) -> bool {
    matches!(node.kind(), "string_literal" | "character_literal")
}

pub(super) fn java_is_import(node: Node<'_>, _source: &str) -> bool {
    node.kind() == "import_declaration"
}
