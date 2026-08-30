//! Rust adapter: AST classification, symbols, context, and atomic ranges.

use std::ops::Range;

use tree_sitter::Node;

use super::{field_symbol_info, sibling_context, SymbolInfo};
use crate::core::AtomKind;

pub(super) fn rust_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
    match node.kind() {
        "function_item" | "function_signature_item" => Some(AtomKind::Function),
        "struct_item" | "enum_item" | "union_item" => Some(AtomKind::Class),
        "trait_item" | "impl_item" | "mod_item" | "foreign_mod_item" | "type_item" => {
            Some(AtomKind::Module)
        }
        "const_item"
        | "static_item"
        | "use_declaration"
        | "extern_crate_declaration"
        | "macro_definition"
        | "macro_invocation" => Some(AtomKind::Declaration),
        "line_comment" | "block_comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

pub(super) fn rust_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    if node.kind() == "impl_item" {
        let target = node
            .child_by_field_name("type")
            .map(|child| source[child.byte_range()].trim())?;
        return Some(SymbolInfo::plain(match node.child_by_field_name("trait") {
            Some(trait_node) => format!(
                "impl {} for {target}",
                source[trait_node.byte_range()].trim()
            ),
            None => format!("impl {target}"),
        }));
    }
    field_symbol_info(node, source)
}

pub(super) fn rust_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    let range = sibling_context(node, source, &["line_comment", "block_comment"])?;
    let text = source.get(range.clone())?;
    let last_line = text.lines().next_back().unwrap_or_default().trim_start();
    (last_line.starts_with("///") || last_line.starts_with("//!")).then_some(range)
}

pub(super) fn rust_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string_literal" | "raw_string_literal" | "macro_definition"
    )
}
