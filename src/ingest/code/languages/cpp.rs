//! C++ adapter: AST classification, symbols, context, and atomic ranges.

use std::ops::Range;

use tree_sitter::Node;

use super::{
    declaration_kind, declarator_name, in_function_body, sibling_context,
    specifier_is_named_definition, SymbolInfo,
};
use crate::core::AtomKind;

pub(super) fn cpp_interesting(node: Node<'_>, _source: &str) -> Option<AtomKind> {
    let kind = match node.kind() {
        "namespace_definition" => AtomKind::Module,
        "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier" => {
            // Only named definitions become Class atoms; type-only uses and
            // anonymous bodies stay out of the symbol table.
            specifier_is_named_definition(node).then_some(AtomKind::Class)?
        }
        "function_definition" => AtomKind::Function,
        "declaration" | "field_declaration" => {
            // Constructors, destructors, and operators arrive as plain
            // declarations whose declarator names a function; class member
            // declarations are field_declaration nodes.
            let declarator = node.child_by_field_name("declarator")?;
            Some(declaration_kind(declarator))?
        }
        "alias_declaration"
        | "type_definition"
        | "concept_definition"
        | "using_declaration"
        | "namespace_alias_definition"
        | "preproc_include"
        | "preproc_def"
        | "preproc_function_def" => AtomKind::Declaration,
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

pub(super) fn cpp_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    match node.kind() {
        "function_definition" | "type_definition" | "declaration" | "field_declaration" => {
            declarator_name(node.child_by_field_name("declarator")?, source)
        }
        "namespace_definition"
        | "alias_declaration"
        | "concept_definition"
        | "namespace_alias_definition" => {
            let name = node.child_by_field_name("name")?;
            Some(SymbolInfo::plain(
                source[name.byte_range()].trim().to_string(),
            ))
        }
        "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier" => {
            let name = node.child_by_field_name("name")?;
            let text = source[name.byte_range()].trim();
            (!text.is_empty()).then(|| SymbolInfo::plain(text.to_string()))
        }
        "using_declaration" => {
            let declared = node.named_child(0)?;
            let text = source[declared.byte_range()].trim();
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

pub(super) fn cpp_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    // The declaration inside a template carries the template preamble as
    // its context; doc comments above the template join it. The
    // template_declaration itself is never emitted.
    if let Some(parent) = node.parent() {
        if parent.kind() == "template_declaration" {
            let start = sibling_context(parent, source, &["comment"])
                .map_or(parent.start_byte(), |range| range.start);
            return Some(start..node.start_byte());
        }
    }
    sibling_context(node, source, &["comment"])
}

pub(super) fn cpp_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string_literal" | "char_literal" | "raw_string_literal" | "preproc_arg"
    )
}
