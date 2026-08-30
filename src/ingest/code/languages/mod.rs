//! Language adapters: per-language AST classification, symbols, context,
//! and atomic ranges. The extension registry lives in `registry`.

use std::ops::Range;

use tree_sitter::Node;

use crate::core::AtomKind;

mod c;
mod cpp;
mod csharp;
mod ecmascript;
mod go;
mod java;
mod php;
mod python;
mod registry;
mod ruby;
mod rust;
mod shell;

use c::{c_atomic, c_interesting, c_is_import, c_leading_context, c_symbol_info};
use cpp::{cpp_atomic, cpp_interesting, cpp_is_import, cpp_leading_context, cpp_symbol_info};
use csharp::{
    csharp_atomic, csharp_interesting, csharp_is_import, csharp_leading_context, csharp_symbol_info,
};
use ecmascript::{
    ecmascript_atomic, ecmascript_interesting, ecmascript_is_import, ecmascript_leading_context,
    ecmascript_symbol_info,
};
use go::{go_atomic, go_interesting, go_is_import, go_leading_context, go_symbol_info};
use java::{java_atomic, java_interesting, java_is_import, java_leading_context, java_symbol_info};
use php::{php_atomic, php_interesting, php_is_import, php_leading_context, php_symbol_info};
use python::{python_atomic, python_interesting, python_is_import, python_leading_context};
use ruby::{ruby_atomic, ruby_interesting, ruby_is_import, ruby_leading_context, ruby_symbol_info};
use rust::{rust_atomic, rust_interesting, rust_is_import, rust_leading_context, rust_symbol_info};
use shell::{shell_atomic, shell_interesting, shell_is_import, shell_leading_context};

pub use registry::{
    code_extension, language_for_source, language_name, supports_code_path, Language,
};

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

pub(super) fn field_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    first_field_text(node, source, &["name", "type"]).map(SymbolInfo::plain)
}

pub(super) fn first_field_text(node: Node<'_>, source: &str, fields: &[&str]) -> Option<String> {
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

/// Walk a C-family declarator to its innermost identifier.
///
/// Recursion covers the declarator wrappers; C++ `scope::name` composes
/// the qualified component. A function pointer yields the variable
/// identity so adapters can classify it as a plain declaration.
pub(super) fn declarator_name(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    match node.kind() {
        "identifier" | "type_identifier" | "field_identifier" => {
            let name = source[node.byte_range()].trim();
            (!name.is_empty()).then(|| SymbolInfo::plain(name.to_string()))
        }
        "qualified_identifier" => {
            let scope = node.child_by_field_name("scope")?;
            let name = node.child_by_field_name("name")?;
            let display = declarator_name(name, source)
                .map(|info| info.display_name)
                .or_else(|| {
                    let text = source[name.byte_range()].trim();
                    (!text.is_empty()).then(|| text.to_string())
                })?;
            let scope_text = source[scope.byte_range()].trim();
            (!scope_text.is_empty()).then(|| SymbolInfo {
                display_name: display.clone(),
                qualified_component: format!("{scope_text}::{display}"),
            })
        }
        "operator_name" | "destructor_name" => {
            let text = source[node.byte_range()].trim();
            (!text.is_empty()).then(|| SymbolInfo::plain(text.to_string()))
        }
        "operator_cast" => {
            let type_node = node.named_child(0)?;
            let text = source[type_node.byte_range()].trim();
            (!text.is_empty()).then(|| SymbolInfo::plain(format!("operator {text}")))
        }
        "function_declarator"
        | "pointer_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "attributed_declarator"
        | "init_declarator"
        | "pointer_type_declarator"
        | "reference_declarator" => node
            .child_by_field_name("declarator")
            .or_else(|| node.named_child(0))
            .and_then(|child| declarator_name(child, source)),
        _ => None,
    }
}

/// A declaration is a Function only when its declarator names a function
/// directly. Function-pointer variables wrap the name in a parenthesized
/// declarator and stay plain declarations.
pub(super) fn declaration_kind(declarator: Node<'_>) -> AtomKind {
    let mut current = Some(declarator);
    while let Some(node) = current {
        if node.kind() == "function_declarator" {
            let is_function_pointer = matches!(
                node.child_by_field_name("declarator").map(|n| n.kind()),
                Some("parenthesized_declarator")
            );
            return if is_function_pointer {
                AtomKind::Declaration
            } else {
                AtomKind::Function
            };
        }
        if node.kind() == "operator_cast" {
            // A conversion operator declares a function through its own
            // name; its declarator only carries the parameter list.
            return AtomKind::Function;
        }
        current = node.child_by_field_name("declarator");
    }
    AtomKind::Declaration
}

pub(super) fn specifier_is_named_definition(node: Node<'_>) -> bool {
    node.child_by_field_name("name").is_some() && node.child_by_field_name("body").is_some()
}

pub(super) fn in_function_body(node: Node<'_>) -> bool {
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if current.kind() == "function_definition" {
            return true;
        }
        ancestor = current.parent();
    }
    false
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
