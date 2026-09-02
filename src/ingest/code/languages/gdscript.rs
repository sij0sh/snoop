//! GDScript adapter: AST classification, symbols, context, and atomic
//! ranges. Targets Godot 4.x. Static `res://` references (path extends,
//! `preload`, `load`) become import declarations; expressions are never
//! evaluated and arbitrary strings are never mined.

use std::ops::Range;

use tree_sitter::Node;

use super::{field_symbol_info, sibling_context, SymbolInfo};
use crate::core::AtomKind;

pub(super) fn gdscript_interesting(node: Node<'_>, source: &str) -> Option<AtomKind> {
    let kind = match node.kind() {
        "class_definition" => AtomKind::Class,
        "function_definition" | "constructor_definition" => AtomKind::Function,
        "class_name_statement" => AtomKind::Declaration,
        "signal_statement"
        | "enum_definition"
        | "const_statement"
        | "variable_statement"
        | "export_variable_statement"
        | "onready_variable_statement" => {
            // Local variables and constants live inside function and
            // accessor bodies; only script-level declarations are units.
            if in_callable_body(node) {
                return None;
            }
            AtomKind::Declaration
        }
        "extends_statement" => has_string_child(node).then_some(AtomKind::Declaration)?,
        "call" => static_load_call(node, source).map(|_| AtomKind::Declaration)?,
        "comment" => AtomKind::Comment,
        _ => return None,
    };
    Some(kind)
}

pub(super) fn gdscript_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    let mut info = match node.kind() {
        // `_init` has no `name` field; the constructor keyword is its name.
        "constructor_definition" => SymbolInfo::plain("_init".to_string()),
        "extends_statement" => path_extend_identity(node, source)?,
        "call" => static_load_call(node, source)?,
        _ => field_symbol_info(node, source)?,
    };
    if node.kind() != "class_name_statement" {
        if let Some(script_name) = script_class_name(node, source) {
            info.qualified_component = format!("{script_name} > {}", info.qualified_component);
        }
    }
    Some(info)
}

/// A script's `class_name` qualifies its top-level symbols. Members of
/// inner classes resolve through the enclosing breadcrumb instead, so
/// the sibling walk naturally stops at the `class_body` boundary.
fn script_class_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut sibling = node.prev_named_sibling();
    while let Some(previous) = sibling {
        if previous.kind() == "class_name_statement" {
            let name = previous.child_by_field_name("name")?;
            let text = source[name.byte_range()].trim();
            return (!text.is_empty()).then(|| text.to_string());
        }
        sibling = previous.prev_named_sibling();
    }
    None
}

fn path_extend_identity(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    let literal = string_child(node, source)?;
    Some(SymbolInfo::plain(format!("extends {literal}")))
}

/// Only `preload("res://...")` and `load("res://...")` with a literal
/// string argument qualify. Variables, concatenations, and non-Godot
/// paths stay unrecognized.
fn static_load_call(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    let callee = node.named_child(0)?;
    let name = source[callee.byte_range()].trim();
    if name != "preload" && name != "load" {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    let argument = arguments.named_child(0)?;
    if argument.kind() != "string" {
        return None;
    }
    let path = string_text(&source[argument.byte_range()]);
    path.starts_with("res://")
        .then(|| SymbolInfo::plain(format!("{name} {path}")))
}

fn has_string_child(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .any(|child| child.kind() == "string");
    found
}

fn string_child(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let literal = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "string")?;
    Some(string_text(&source[literal.byte_range()]))
}

/// Strip the outer quote run (`, ', or triple variants) from a string
/// literal. Inner content is kept verbatim.
fn string_text(literal: &str) -> String {
    let trimmed = literal.trim();
    for quote in ["\"\"\"", "'''"] {
        if let Some(body) = trimmed.strip_prefix(quote) {
            return body.strip_suffix(quote).unwrap_or(body).to_string();
        }
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && (bytes[0] == b'"' || bytes[0] == b'\'') {
        let quote = bytes[0] as char;
        if let Some(body) = trimmed.strip_prefix(quote) {
            return body.strip_suffix(quote).unwrap_or(body).to_string();
        }
    }
    trimmed.to_string()
}

/// Declarations nested in functions, constructors, lambdas, and
/// accessors are local state, not script surface.
fn in_callable_body(node: Node<'_>) -> bool {
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if matches!(
            current.kind(),
            "function_definition" | "constructor_definition" | "lambda" | "get_body" | "set_body"
        ) {
            return true;
        }
        ancestor = current.parent();
    }
    false
}

pub(super) fn gdscript_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    // `##` doc comments and bare annotations (`@rpc`, `@export_group`)
    // are siblings of functions and classes; variable annotations are
    // children of the statement and need no sibling walk.
    sibling_context(node, source, &["comment", "annotation", "annotations"])
}

pub(super) fn gdscript_atomic(node: Node<'_>) -> bool {
    matches!(node.kind(), "string" | "string_name" | "node_path")
}

pub(super) fn gdscript_is_import(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "extends_statement" => has_string_child(node),
        "call" => static_load_call(node, source).is_some(),
        _ => false,
    }
}
