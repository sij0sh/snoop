//! Godot resource adapter: one section-based parser for `.tscn` scenes
//! and `.tres` text resources. Sections are the retrieval units; node
//! sections keep their properties, header and `ext_resource` sections
//! stay in the file overview, and oversized serialized values are split
//! through the atomic-range chunker instead of being interpreted.

use std::ops::Range;

use tree_sitter::Node;

use super::{sibling_context, SymbolInfo};
use crate::core::AtomKind;

pub(super) fn godot_resource_interesting(node: Node<'_>, source: &str) -> Option<AtomKind> {
    match node.kind() {
        "section" => section_kind(node, source),
        "comment" => Some(AtomKind::Comment),
        _ => None,
    }
}

fn section_kind(section: Node<'_>, source: &str) -> Option<AtomKind> {
    match section_name(section, source)? {
        "node" => Some(AtomKind::Class),
        // The main `[resource]` and substantial `[sub_resource]` blocks
        // carry searchable configuration; `[gd_scene]`, `[gd_resource]`,
        // and `[ext_resource]` headers stay in the file overview.
        "resource" | "connection" => Some(AtomKind::Declaration),
        "sub_resource" => has_property(section).then_some(AtomKind::Declaration),
        _ => None,
    }
}

pub(super) fn godot_resource_symbol_info(node: Node<'_>, source: &str) -> Option<SymbolInfo> {
    match section_name(node, source)? {
        "node" => node_identity(node, source),
        "sub_resource" => sub_resource_identity(node, source),
        "connection" => connection_identity(node, source),
        "resource" => Some(SymbolInfo::plain("resource".to_string())),
        _ => None,
    }
}

/// The section keyword is the first named child (`node`,
/// `ext_resource`, `sub_resource`, `connection`, `resource`, ...).
fn section_name<'s>(section: Node<'_>, source: &'s str) -> Option<&'s str> {
    let identifier = section.named_child(0).filter(|child| child.kind() == "identifier")?;
    let name = source[identifier.byte_range()].trim();
    (!name.is_empty()).then_some(name)
}

/// The scene-relative node path: `parent` attributes are already
/// scene-relative paths, so joining them with the node name is exact.
/// Instanced and inherited trees are never expanded into the path.
fn node_identity(section: Node<'_>, source: &str) -> Option<SymbolInfo> {
    let name = attribute(section, "name", source)?;
    let path = match attribute(section, "parent", source).as_deref() {
        None | Some("") | Some(".") => name,
        Some(parent) => format!("{}/{}", parent.trim_start_matches("./"), name),
    };
    Some(SymbolInfo::plain(path))
}

/// Subresource IDs are file-local and stay namespaced under the
/// containing file's breadcrumb: `SubResource:Type:Id`.
fn sub_resource_identity(section: Node<'_>, source: &str) -> Option<SymbolInfo> {
    let id = attribute(section, "id", source)?;
    let identity = match attribute(section, "type", source) {
        Some(kind) if !kind.is_empty() => format!("SubResource:{kind}:{id}"),
        _ => format!("SubResource:{id}"),
    };
    Some(SymbolInfo::plain(identity))
}

/// Signal wiring renders as the routing text Snoop's overview format
/// uses: `from.signal -> method`.
fn connection_identity(section: Node<'_>, source: &str) -> Option<SymbolInfo> {
    let signal = attribute(section, "signal", source)?;
    let from = attribute(section, "from", source).unwrap_or_default();
    let method = attribute(section, "method", source)?;
    Some(SymbolInfo::plain(format!(
        "{from}.{signal} -> {method}"
    )))
}

fn attribute(section: Node<'_>, key: &str, source: &str) -> Option<String> {
    let mut cursor = section.walk();
    for child in section.named_children(&mut cursor) {
        if child.kind() != "attribute" {
            continue;
        }
        let mut inner = child.walk();
        let mut parts = child.named_children(&mut inner);
        let name = parts.next()?;
        if name.kind() == "identifier" && source[name.byte_range()].trim() == key {
            let value = parts.next()?;
            return Some(string_text(&source[value.byte_range()]));
        }
    }
    None
}

fn has_property(section: Node<'_>) -> bool {
    let mut cursor = section.walk();
    let found = section
        .named_children(&mut cursor)
        .any(|child| child.kind() == "property");
    found
}

/// Strip outer quotes from a string attribute value; other values pass
/// through verbatim.
fn string_text(value: &str) -> String {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        return trimmed[1..trimmed.len() - 1].to_string();
    }
    trimmed.to_string()
}

pub(super) fn godot_resource_leading_context(node: Node<'_>, source: &str) -> Option<Range<usize>> {
    sibling_context(node, source, &["comment"])
}

pub(super) fn godot_resource_atomic(node: Node<'_>) -> bool {
    // Large serialized values (tile maps, animations, packed arrays)
    // become chunk boundaries so units never embed thousands of numbers.
    matches!(node.kind(), "string" | "array" | "dictionary")
}

pub(super) fn godot_resource_is_import(_node: Node<'_>, _source: &str) -> bool {
    // ExtResource and SubResource IDs are file-local; they must never
    // become global import anchors.
    false
}
