use super::{
    code_anchors, code_routing, content_atom, file_anchor, make_unit, prepend_leading_context,
    whole_atom_evidence, BuiltUnit, ParsedAtom, UnitKind,
};

const IMPORT_NODE_KINDS: &[&str] = &[
    "use_declaration",
    "extern_crate_declaration",
    "import_statement",
    "import_declaration",
    "preproc_include",
    "using_directive",
    "namespace_use_declaration",
];

fn is_import_atom(atom: &ParsedAtom) -> bool {
    // Adapters own import detection; fall back to node kinds for atoms
    // produced before the callback existed.
    if let Some(flag) = atom.metadata["is_import"].as_bool() {
        return flag;
    }
    atom.metadata["node_kind"]
        .as_str()
        .is_some_and(|kind| IMPORT_NODE_KINDS.contains(&kind))
}

fn comment_is_attached(atoms: &[ParsedAtom], index: usize) -> bool {
    let atom = &atoms[index];
    atoms[index + 1..]
        .iter()
        .find(|candidate| {
            content_atom(candidate) && candidate.kind != crate::core::AtomKind::Comment
        })
        .is_some_and(|next| {
            next.metadata["leading_context"]["start_offset"].as_u64()
                == Some(atom.start_offset as u64)
                && next.metadata["leading_context"]["end_offset"].as_u64()
                    == Some(atom.end_offset as u64)
        })
}

fn comment_is_standalone_unit(atoms: &[ParsedAtom], index: usize) -> bool {
    if comment_is_attached(atoms, index) {
        return false;
    }
    let text = &atoms[index].text;
    text.lines().count() >= 2 || text.chars().count() >= 60
}

fn shell_children(atoms: &[ParsedAtom], index: usize) -> Vec<usize> {
    atoms
        .iter()
        .enumerate()
        .filter(|(candidate, atom)| {
            *candidate != index
                && atom.parent_index == Some(index)
                && !is_import_atom(atom)
                && (atom.kind != crate::core::AtomKind::Comment
                    || comment_is_standalone_unit(atoms, *candidate))
        })
        .map(|(candidate, _)| candidate)
        .collect()
}

fn shell_header(atoms: &[ParsedAtom], index: usize, children: &[usize]) -> String {
    let atom = &atoms[index];
    let Some(first_child) = children
        .iter()
        .map(|child| atoms[*child].start_offset)
        .min()
    else {
        return String::new();
    };
    let end = first_child
        .saturating_sub(atom.start_offset)
        .min(atom.text.len());
    atom.text[..end].trim_end().to_string()
}

fn shell_evidence(atoms: &[ParsedAtom], index: usize, children: &[usize]) -> String {
    let atom = &atoms[index];
    let mut evidence = String::new();
    prepend_leading_context(&mut evidence, atom);
    evidence.push_str(&atom.breadcrumb);
    evidence.push_str("\n\n");
    let header = shell_header(atoms, index, children);
    if !header.is_empty() {
        evidence.push_str(&header);
        evidence.push('\n');
    }
    for (position, child) in children.iter().enumerate() {
        let child_atom = &atoms[*child];
        let signature = child_atom.metadata["signature"]
            .as_str()
            .unwrap_or_default();
        let line = signature.split('{').next().unwrap_or(signature).trim();
        if line.is_empty() {
            let name = child_atom.breadcrumb.rsplit(" > ").next().unwrap_or_default();
            evidence.push_str(name);
        } else {
            evidence.push_str(line);
        }
        evidence.push('\n');
        // Preserve the text between this child and the next one so
        // scripts keep their top-level orchestration visible.
        let next_start = children
            .get(position + 1)
            .map(|next| atoms[*next].start_offset)
            .unwrap_or(atom.end_offset);
        let gap_start = child_atom.end_offset.saturating_sub(atom.start_offset);
        let gap_end = next_start
            .saturating_sub(atom.start_offset)
            .min(atom.text.len());
        if gap_start < gap_end {
            if let Some(gap) = atom.text.get(gap_start..gap_end) {
                let gap = gap.trim();
                if !gap.is_empty() {
                    evidence.push_str(gap);
                    evidence.push('\n');
                }
            }
        }
    }
    evidence
}

fn imports_unit(
    atoms: &[ParsedAtom],
    locator: &str,
    import_indices: &[usize],
) -> Option<BuiltUnit> {
    if import_indices.is_empty() {
        return None;
    }
    let body = import_indices
        .iter()
        .map(|index| atoms[*index].text.trim())
        .collect::<Vec<_>>()
        .join("\n");
    let evidence = format!("{locator} > imports\n\n{body}");
    let routing =
        format!("source: code\npath: {locator}\nsymbol: {locator} > imports\nkind: imports");
    let mut unit = make_unit(
        UnitKind::Code,
        import_indices,
        atoms,
        evidence,
        routing,
        vec![file_anchor(locator, "defined_in")],
    );
    unit.metadata["unit_shape"] = serde_json::json!("imports");
    Some(unit)
}

pub(super) fn build_code(atoms: &[ParsedAtom], locator: &str) -> Vec<BuiltUnit> {
    let mut units = Vec::new();
    let mut imports = Vec::new();
    for (index, atom) in atoms
        .iter()
        .enumerate()
        .filter(|(_, atom)| content_atom(atom))
    {
        if is_import_atom(atom) {
            imports.push(index);
            continue;
        }
        if atom.kind == crate::core::AtomKind::Comment && !comment_is_standalone_unit(atoms, index)
        {
            continue;
        }
        let routing = code_routing(locator, atom);
        let segments = atom.metadata["chunk_segments"].as_array();
        if let Some(segments) = segments.filter(|segments| !segments.is_empty()) {
            let anchors = code_anchors(locator, atom);
            for segment in segments {
                let Some(start) = segment["start_offset"].as_u64().map(|value| value as usize)
                else {
                    continue;
                };
                let Some(end) = segment["end_offset"].as_u64().map(|value| value as usize) else {
                    continue;
                };
                let relative_start = start.saturating_sub(atom.start_offset);
                let relative_end = end.saturating_sub(atom.start_offset);
                let Some(text) = atom.text.get(relative_start..relative_end) else {
                    continue;
                };
                let evidence = format!("{}\n\n{}", atom.breadcrumb, text);
                let mut unit = make_unit(
                    UnitKind::Code,
                    &[index],
                    atoms,
                    evidence,
                    routing.clone(),
                    anchors.clone(),
                );
                unit.metadata["source_slices"] = serde_json::json!([{
                    "atom_hash": atom.content_hash,
                    "start_offset": segment["start_offset"],
                    "end_offset": segment["end_offset"],
                    "boundary": segment["boundary"],
                }]);
                units.push(unit);
            }
        } else {
            let children = shell_children(atoms, index);
            let evidence = if children.is_empty() {
                whole_atom_evidence(atom)
            } else {
                shell_evidence(atoms, index, &children)
            };
            let mut unit = make_unit(
                UnitKind::Code,
                &[index],
                atoms,
                evidence,
                routing,
                code_anchors(locator, atom),
            );
            if !children.is_empty() {
                let child_names: Vec<String> = children
                    .iter()
                    .map(|child| atoms[*child].breadcrumb.clone())
                    .collect();
                unit.metadata["unit_shape"] = serde_json::json!("shell");
                unit.metadata["elided_children"] = serde_json::json!(child_names);
            }
            units.push(unit);
        }
    }
    if let Some(unit) = imports_unit(atoms, locator, &imports) {
        units.insert(0, unit);
    }
    units
}
