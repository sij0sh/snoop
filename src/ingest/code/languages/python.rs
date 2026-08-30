//! Python adapter: AST classification, context, and atomic ranges.

use std::ops::Range;

use tree_sitter::Node;

use super::sibling_context;
use crate::core::AtomKind;

pub(super) fn python_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
    match node.kind() {
        "function_definition" => Some(AtomKind::Function),
        "class_definition" => Some(AtomKind::Class),
        "type_alias_statement" | "import_statement" => Some(AtomKind::Declaration),
        "comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

pub(super) fn python_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
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

pub(super) fn python_atomic(node: Node<'_>) -> bool {
    matches!(node.kind(), "string" | "concatenated_string")
}

pub(super) fn python_is_import(node: Node<'_>, _source: &str) -> bool {
    node.kind() == "import_statement"
}
