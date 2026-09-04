//! ECMAScript adapter (TypeScript, TSX, JavaScript, JSX): AST
//! classification, symbols, context, and atomic ranges. Static Tauri
//! bridge calls (`invoke`, `emit`, `listen`) with a literal-string
//! command become named declarations; dynamic arguments are never
//! extracted.

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
        "call_expression" => static_bridge_call(node, source).map(|_| AtomKind::Declaration),
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
    if node.kind() == "call_expression" {
        return static_bridge_call(node, source);
    }
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

/// The display name is the bare command or event name so the symbol
/// anchor exact-matches the Rust-side command; the breadcrumb keeps the
/// call name for readability. Aliased imports, member callees with
/// other names, variables, template literals, and concatenations stay
/// unrecognized.
fn static_bridge_call(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    let function = node.child_by_field_name("function")?;
    let callee = match function.kind() {
        "identifier" => function,
        "member_expression" => function.child_by_field_name("property")?,
        _ => return None,
    };
    let call_name = source[callee.byte_range()].trim();
    if !matches!(call_name, "invoke" | "emit" | "listen") {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    let argument = arguments.named_child(0)?;
    if argument.kind() != "string" {
        return None;
    }
    let command = string_text(&source[argument.byte_range()]);
    if command.is_empty() {
        return None;
    }
    Some(SymbolInfo {
        display_name: command.clone(),
        qualified_component: format!("{call_name} {command}"),
    })
}

fn string_text(literal: &str) -> String {
    let trimmed = literal.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && (bytes[0] == b'"' || bytes[0] == b'\'') {
        let quote = bytes[0] as char;
        if let Some(body) = trimmed.strip_prefix(quote) {
            if let Some(body) = body.strip_suffix(quote) {
                return body.to_string();
            }
        }
    }
    trimmed.to_string()
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
