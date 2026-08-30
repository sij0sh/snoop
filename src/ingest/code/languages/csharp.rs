//! C# adapter: AST classification, symbols, context, and atomic ranges.

use std::ops::Range;

use tree_sitter::Node;

use super::{field_symbol_info, sibling_context, SymbolInfo};
use crate::core::AtomKind;

pub(super) fn csharp_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
    match node.kind() {
        "namespace_declaration" | "file_scoped_namespace_declaration" => Some(AtomKind::Module),
        "class_declaration"
        | "struct_declaration"
        | "interface_declaration"
        | "record_declaration"
        | "enum_declaration" => Some(AtomKind::Class),
        "method_declaration"
        | "constructor_declaration"
        | "destructor_declaration"
        | "operator_declaration"
        | "conversion_operator_declaration" => Some(AtomKind::Function),
        "property_declaration"
        | "event_declaration"
        | "event_field_declaration"
        | "delegate_declaration"
        | "field_declaration"
        | "using_directive" => Some(AtomKind::Declaration),
        "comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

pub(super) fn csharp_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    let mut info = match node.kind() {
        "operator_declaration" => node.child_by_field_name("operator").map(|token| {
            SymbolInfo::plain(format!("operator {}", source[token.byte_range()].trim()))
        }),
        "conversion_operator_declaration" => node.child_by_field_name("type").map(|target| {
            SymbolInfo::plain(format!("operator {}", source[target.byte_range()].trim()))
        }),
        "field_declaration" | "event_field_declaration" => {
            csharp_field_names(node, source).map(SymbolInfo::plain)
        }
        _ => field_symbol_info(node, source),
    }?;
    if let Some(namespace) = csharp_namespace_prefix(node, source) {
        info.qualified_component = format!("{namespace} > {}", info.qualified_component);
    }
    Some(info)
}

/// File-scoped namespace members are tree siblings of the namespace node,
/// so the namespace qualifier must be composed manually.
fn csharp_namespace_prefix(node: Node<'_>, source: &str) -> Option<String> {
    let mut sibling = node.prev_named_sibling();
    while let Some(previous) = sibling {
        if previous.kind() == "file_scoped_namespace_declaration" {
            let name = previous.child_by_field_name("name")?;
            let text = source[name.byte_range()].trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
            return None;
        }
        sibling = previous.prev_named_sibling();
    }
    None
}

/// C# field declarations nest variable declarators under
/// `variable_declaration`, unlike Java's flat layout.
fn csharp_field_names(node: Node<'_>, source: &str) -> Option<String> {
    let mut names: Vec<&str> = Vec::new();
    let mut cursor = node.walk();
    for variable in node.named_children(&mut cursor) {
        if variable.kind() != "variable_declaration" {
            continue;
        }
        let mut inner = variable.walk();
        for declarator in variable.named_children(&mut inner) {
            if declarator.kind() != "variable_declarator" {
                continue;
            }
            if let Some(name) = declarator.child_by_field_name("name") {
                let value = source[name.byte_range()].trim();
                if !value.is_empty() {
                    names.push(value);
                }
            }
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

pub(super) fn csharp_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["comment", "attribute_list"])
}

pub(super) fn csharp_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string_literal"
            | "verbatim_string_literal"
            | "raw_string_literal"
            | "character_literal"
            | "interpolated_string_expression"
    )
}

pub(super) fn csharp_is_import(node: Node<'_>, _source: &str) -> bool {
    node.kind() == "using_directive"
}
