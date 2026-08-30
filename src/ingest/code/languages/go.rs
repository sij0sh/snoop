//! Go adapter: AST classification, symbols, context, and atomic ranges.

use std::ops::Range;

use tree_sitter::Node;

use super::{field_symbol_info, first_field_text, sibling_context, SymbolInfo};
use crate::core::AtomKind;

pub(super) fn go_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
    match node.kind() {
        "function_declaration" | "method_declaration" => Some(AtomKind::Function),
        "type_spec" => {
            let aggregate = node
                .child_by_field_name("type")
                .is_some_and(|value| matches!(value.kind(), "struct_type" | "interface_type"));
            Some(if aggregate {
                AtomKind::Class
            } else {
                AtomKind::Declaration
            })
        }
        "type_alias" | "const_spec" | "var_spec" | "import_spec" => Some(AtomKind::Declaration),
        "comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

pub(super) fn go_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    match node.kind() {
        "method_declaration" => {
            let name = node.child_by_field_name("name")?;
            let receiver = node.child_by_field_name("receiver")?;
            let method = source[name.byte_range()].trim();
            let base = source[receiver.byte_range()]
                .split_whitespace()
                .next_back()?
                .trim_matches(|character: char| matches!(character, '(' | ')' | '*'))
                .trim();
            if base.is_empty() || method.is_empty() {
                return None;
            }
            Some(SymbolInfo::plain(format!("{base}.{method}")))
        }
        "import_spec" => first_field_text(node, source, &["name", "path"]).map(SymbolInfo::plain),
        _ => field_symbol_info(node, source),
    }
}

pub(super) fn go_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["comment"])
}

pub(super) fn go_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "interpreted_string_literal" | "raw_string_literal" | "rune_literal"
    )
}
