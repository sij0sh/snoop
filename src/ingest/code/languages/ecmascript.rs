//! ECMAScript adapter (TypeScript, TSX, JavaScript, JSX): AST
//! classification, symbols, context, and atomic ranges.

use std::ops::Range;

use tree_sitter::Node;

use super::{field_symbol_info, sibling_context, SymbolInfo};
use crate::core::AtomKind;

pub(super) fn ecmascript_interesting(node: Node<'_>, source: &str) -> Option<AtomKind> {
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

pub(super) fn ecmascript_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
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

pub(super) fn ecmascript_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["comment"])
}

pub(super) fn ecmascript_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string" | "template_string" | "regex" | "jsx_element" | "jsx_fragment"
    )
}

pub(super) fn ecmascript_is_import(node: Node<'_>, _source: &str) -> bool {
    node.kind() == "import_statement"
}
