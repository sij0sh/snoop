//! C adapter: AST classification, symbols, context, and atomic ranges.

use std::ops::Range;

use tree_sitter::Node;

use super::{declarator_name, sibling_context, SymbolInfo};
use crate::core::AtomKind;

pub(super) fn c_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
    let kind = match node.kind() {
        "function_definition" => AtomKind::Function,
        "struct_specifier" | "union_specifier" | "enum_specifier" => {
            // Only named definitions become Class atoms; type-only uses and
            // anonymous bodies stay out of the symbol table.
            specifier_is_named_definition(node).then_some(AtomKind::Class)?
        }
        "type_definition" => AtomKind::Declaration,
        "declaration" => {
            let declarator = node.child_by_field_name("declarator")?;
            Some(declaration_kind(declarator))?
        }
        "preproc_include" | "preproc_def" | "preproc_function_def" => AtomKind::Declaration,
        "comment" => AtomKind::Comment,
        _ => return None,
    };
    let is_preprocessor = matches!(
        node.kind(),
        "preproc_include" | "preproc_def" | "preproc_function_def"
    );
    if kind != AtomKind::Comment && !is_preprocessor && in_function_body(node) {
        return None;
    }
    Some(kind)
}

pub(super) fn c_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    match node.kind() {
        "function_definition" | "type_definition" | "declaration" => {
            declarator_name(node.child_by_field_name("declarator")?, source)
        }
        "struct_specifier" | "union_specifier" | "enum_specifier" => {
            let name = node.child_by_field_name("name")?;
            let text = source[name.byte_range()].trim();
            (!text.is_empty()).then(|| SymbolInfo::plain(text.to_string()))
        }
        "preproc_include" => {
            let path = node.child_by_field_name("path")?;
            Some(SymbolInfo::plain(
                source[path.byte_range()].trim().to_string(),
            ))
        }
        "preproc_def" | "preproc_function_def" => {
            let name = node.child_by_field_name("name")?;
            Some(SymbolInfo::plain(
                source[name.byte_range()].trim().to_string(),
            ))
        }
        _ => None,
    }
}

pub(super) fn c_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["comment"])
}

pub(super) fn c_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string_literal" | "char_literal" | "preproc_arg"
    )
}

/// A declaration is a Function only when its declarator names a function
/// directly. Function-pointer variables wrap the name in a parenthesized
/// declarator and stay plain declarations.
fn declaration_kind(declarator: Node<'_>) -> AtomKind {
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
        current = node.child_by_field_name("declarator");
    }
    AtomKind::Declaration
}

fn specifier_is_named_definition(node: Node<'_>) -> bool {
    node.child_by_field_name("name").is_some() && node.child_by_field_name("body").is_some()
}

fn in_function_body(node: Node<'_>) -> bool {
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if current.kind() == "function_definition" {
            return true;
        }
        ancestor = current.parent();
    }
    false
}
