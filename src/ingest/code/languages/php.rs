//! PHP adapter: AST classification, context, and atomic ranges. Symbol
//! identity stays with the shared field helper; the mixed PHP+HTML
//! grammar keeps templates parseable, and HTML text never becomes a
//! code atom.

use std::ops::Range;

use tree_sitter::Node;

use super::{field_symbol_info, sibling_context, SymbolInfo};
use crate::core::AtomKind;

pub(super) fn php_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    let mut info = match node.kind() {
        "namespace_use_declaration" => {
            let text = source[node.byte_range()].trim().trim_end_matches(';');
            let text = text.strip_prefix("use ").unwrap_or(text);
            (!text.is_empty()).then(|| SymbolInfo::plain(text.to_string()))
        }
        _ => field_symbol_info(node, source),
    }?;
    if let Some(namespace) = php_namespace_prefix(node, source) {
        info.qualified_component = format!("{namespace} > {}", info.qualified_component);
    }
    Some(info)
}

/// Semicolon-style namespaces (`namespace App\\Auth;`) keep their members
/// as tree siblings, so the qualifier must be composed manually. Braced
/// namespaces nest their members and need no help.
fn php_namespace_prefix(node: Node<'_>, source: &str) -> Option<String> {
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if current.kind() == "namespace_definition" {
            return None;
        }
        ancestor = current.parent();
    }
    let mut sibling = node.prev_named_sibling();
    while let Some(previous) = sibling {
        if previous.kind() == "namespace_definition" {
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

pub(super) fn php_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
    match node.kind() {
        "namespace_definition" => Some(AtomKind::Module),
        "class_declaration"
        | "interface_declaration"
        | "trait_declaration"
        | "enum_declaration" => Some(AtomKind::Class),
        "function_definition" | "method_declaration" => Some(AtomKind::Function),
        "property_declaration"
        | "class_constant_declaration"
        | "const_declaration"
        | "namespace_use_declaration" => Some(AtomKind::Declaration),
        "include_expression"
        | "include_once_expression"
        | "require_expression"
        | "require_once_expression" => Some(AtomKind::Declaration),
        "comment" => Some(AtomKind::Comment),
        // Anonymous classes and closures are not named symbols.
        _ => None,
    }
}

pub(super) fn php_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["comment"])
}

pub(super) fn php_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string" | "encapsed_string" | "heredoc_string" | "nowdoc_string"
    )
}

pub(super) fn php_is_import(node: Node<'_>, _source: &str) -> bool {
    matches!(
        node.kind(),
        "namespace_use_declaration"
            | "include_expression"
            | "include_once_expression"
            | "require_expression"
            | "require_once_expression"
    )
}
