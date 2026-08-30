//! Shell adapter (bash): AST classification, context, and atomic
//! ranges. Commands and variable assignments stay inside the root
//! overview; only function definitions, comments, and static source
//! imports become atoms.

use std::ops::Range;

use tree_sitter::Node;

use super::sibling_context;
use crate::core::AtomKind;

pub(super) fn shell_interesting(node: Node<'_>, source: &str) -> Option<AtomKind> {
    match node.kind() {
        "function_definition" => Some(AtomKind::Function),
        "command" => is_source_command(node, source).then_some(AtomKind::Declaration),
        "comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

pub(super) fn shell_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["comment"])
}

pub(super) fn shell_atomic(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string" | "raw_string" | "heredoc_body" | "command_substitution" | "arithmetic_expansion"
    )
}

pub(super) fn shell_is_import(node: Node<'_>, source: &str) -> bool {
    is_source_command(node, source)
}

/// Recognize `source PATH` and `. PATH` only when the path is
/// statically visible. Variables are never expanded or resolved.
fn is_source_command(node: Node<'_>, source: &str) -> bool {
    if node.kind() != "command" {
        return false;
    }
    let Some(name) = node.child_by_field_name("name") else {
        return false;
    };
    let name = source[name.byte_range()].trim();
    if name != "source" && name != "." {
        return false;
    }
    // The path argument must be a literal word or string; a `$`
    // expansion is dynamic and stays unrecognized.
    let mut cursor = node.walk();
    let path = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "word" || child.kind() == "string")
        .map(|argument| source[argument.byte_range()].trim());
    path.is_some_and(|path| !path.is_empty() && !path.contains('$'))
}
