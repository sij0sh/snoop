//! Ruby adapter: AST classification, symbols, context, and atomic
//! ranges. Only explicitly declared constructs become symbols; runtime
//! metaprogramming and framework DSLs are out of scope.

use std::ops::Range;

use tree_sitter::Node;

use super::{field_symbol_info, sibling_context, SymbolInfo};
use crate::core::AtomKind;

const IMPORT_METHODS: &[&str] = &["require", "require_relative", "load"];

pub(super) fn ruby_interesting(node: Node<'_>, source: &str) -> Option<AtomKind> {
    match node.kind() {
        "module" => Some(AtomKind::Module),
        "class" | "singleton_class" => Some(AtomKind::Class),
        "method" | "singleton_method" => Some(AtomKind::Function),
        "alias" => Some(AtomKind::Declaration),
        "call" => is_import_call(node, source).then_some(AtomKind::Declaration),
        "comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

pub(super) fn ruby_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    match node.kind() {
        "singleton_method" => {
            // The anchor stays the bare method name; the breadcrumb keeps
            // the singleton qualification (Session > self.validate).
            let name = node.child_by_field_name("name")?;
            let display = source[name.byte_range()].trim();
            if display.is_empty() {
                return None;
            }
            let qualified = match node.child_by_field_name("object") {
                Some(object) if source[object.byte_range()].trim() == "self" => {
                    format!("self.{display}")
                }
                _ => display.to_string(),
            };
            Some(SymbolInfo {
                display_name: display.to_string(),
                qualified_component: qualified,
            })
        }
        "call" => {
            let method = node.child_by_field_name("method")?;
            Some(SymbolInfo::plain(
                source[method.byte_range()].trim().to_string(),
            ))
        }
        // Class and module names support namespace resolution
        // (class Auth::Session) through the name field text.
        _ => field_symbol_info(node, source),
    }
}

pub(super) fn ruby_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["comment"])
}

pub(super) fn ruby_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string" | "heredoc_body" | "regex" | "simple_symbol" | "delimited_symbol"
    )
}

pub(super) fn ruby_is_import(node: Node<'_>, source: &str) -> bool {
    is_import_call(node, source)
}

fn is_import_call(node: Node<'_>, source: &str) -> bool {
    if node.kind() != "call" {
        return false;
    }
    node.child_by_field_name("method").is_some_and(|method| {
        let name = source[method.byte_range()].trim();
        IMPORT_METHODS.contains(&name)
    })
}
