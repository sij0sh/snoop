use std::collections::HashMap;

use super::{
    estimate_tokens, make_unit, prose_anchors, prose_routing, split_oversized, BuiltUnit,
    ParsedAtom, UnitKind, MAX_TOKENS, MERGE_BELOW,
};
use crate::core::BuiltAnchor;

fn section_key(atoms: &[ParsedAtom], index: usize) -> usize {
    let mut current = index;
    loop {
        if matches!(
            atoms[current].kind,
            crate::core::AtomKind::Heading | crate::core::AtomKind::File
        ) {
            return current;
        }
        match atoms[current].parent_index {
            Some(parent) if parent < current => current = parent,
            _ => return 0,
        }
    }
}

/// A prose unit plus the atom indices needed to rehash it while merging.
struct DraftUnit {
    unit: BuiltUnit,
    indices: Vec<usize>,
}

fn merge_anchor(target: &mut Vec<BuiltAnchor>, anchor: BuiltAnchor) {
    let duplicate = target.iter().any(|existing| {
        existing.kind == anchor.kind
            && existing.value == anchor.value
            && existing.relationship == anchor.relationship
    });
    if !duplicate {
        target.push(anchor);
    }
}

fn push_prose(
    output: &mut Vec<DraftUnit>,
    atoms: &[ParsedAtom],
    locator: &str,
    indices: Vec<usize>,
) {
    if indices.is_empty() {
        return;
    }
    let breadcrumb = atoms[indices[0]].breadcrumb.clone();
    let body = indices
        .iter()
        .map(|index| atoms[*index].text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let evidence = format!("{breadcrumb}\n\n{body}");
    let routing = prose_routing(locator, &breadcrumb, &evidence);
    let anchors = prose_anchors(locator, &evidence);
    let unit = make_unit(UnitKind::Prose, &indices, atoms, evidence, routing, anchors);
    output.push(DraftUnit { unit, indices });
}

pub(super) fn build_prose(atoms: &[ParsedAtom], locator: &str) -> Vec<BuiltUnit> {
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut order = Vec::new();
    for (index, _) in atoms
        .iter()
        .enumerate()
        .filter(|(index, _)| super::content_atom_at(atoms, *index))
    {
        let key = section_key(atoms, index);
        if !groups.contains_key(&key) {
            order.push(key);
        }
        groups.entry(key).or_default().push(index);
    }

    let mut drafts: Vec<DraftUnit> = Vec::new();
    for key in order {
        let mut current = Vec::new();
        for index in groups.remove(&key).unwrap_or_default() {
            let breadcrumb = atoms[index].breadcrumb.clone();
            let single_evidence = format!("{breadcrumb}\n\n{}", atoms[index].text);
            if estimate_tokens(&single_evidence) > MAX_TOKENS {
                push_prose(&mut drafts, atoms, locator, std::mem::take(&mut current));
                let max_chars = (MAX_TOKENS * 4).saturating_sub(breadcrumb.chars().count() + 2);
                let pieces = split_oversized(&atoms[index].text, max_chars);
                let anchors = prose_anchors(locator, &single_evidence);
                for (piece, start, end) in &pieces {
                    let evidence = format!("{breadcrumb}\n\n{piece}");
                    let routing = prose_routing(locator, &breadcrumb, &evidence);
                    let mut unit = make_unit(
                        UnitKind::Prose,
                        &[index],
                        atoms,
                        evidence,
                        routing,
                        anchors.clone(),
                    );
                    unit.metadata["source_slices"] = serde_json::json!([{
                        "atom_hash": atoms[index].content_hash,
                        "start_offset": atoms[index].start_offset + *start,
                        "end_offset": atoms[index].start_offset + *end,
                        "boundary": "prose",
                    }]);
                    drafts.push(DraftUnit {
                        unit,
                        indices: vec![index],
                    });
                }
                continue;
            }
            let mut candidate_indices = current.clone();
            candidate_indices.push(index);
            let candidate_body = candidate_indices
                .iter()
                .map(|item| atoms[*item].text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            let candidate_evidence = format!("{breadcrumb}\n\n{candidate_body}");
            if !current.is_empty() && estimate_tokens(&candidate_evidence) > MAX_TOKENS {
                push_prose(&mut drafts, atoms, locator, std::mem::take(&mut current));
            }
            current.push(index);
        }
        push_prose(&mut drafts, atoms, locator, current);
    }

    let mut merged: Vec<DraftUnit> = Vec::new();
    for draft in drafts {
        if let Some(previous) = merged.last_mut() {
            if draft.unit.token_count < MERGE_BELOW
                && previous.unit.token_count + draft.unit.token_count <= MAX_TOKENS
                && previous.unit.routing_text == draft.unit.routing_text
            {
                previous.indices.extend_from_slice(&draft.indices);
                previous.unit.evidence_text.push_str("\n\n");
                previous.unit.evidence_text.push_str(
                    draft
                        .unit
                        .evidence_text
                        .split_once("\n\n")
                        .map(|(_, body)| body)
                        .unwrap_or(&draft.unit.evidence_text),
                );
                previous.unit.token_count = estimate_tokens(&previous.unit.evidence_text);
                let hashes: Vec<&str> = previous
                    .indices
                    .iter()
                    .map(|index| atoms[*index].content_hash.as_str())
                    .collect();
                previous.unit.content_hash = super::unit_hash(
                    UnitKind::Prose,
                    &hashes,
                    &previous.unit.evidence_text,
                    &previous.unit.routing_text,
                );
                let source_slices: Vec<serde_json::Value> = previous
                    .indices
                    .iter()
                    .map(|index| {
                        serde_json::json!({
                            "atom_hash": atoms[*index].content_hash,
                            "start_offset": atoms[*index].start_offset,
                            "end_offset": atoms[*index].end_offset,
                        })
                    })
                    .collect();
                previous.unit.metadata["source_slices"] = serde_json::json!(source_slices);
                for anchor in draft.unit.anchors {
                    merge_anchor(&mut previous.unit.anchors, anchor);
                }
                continue;
            }
        }
        merged.push(draft);
    }
    merged.into_iter().map(|draft| draft.unit).collect()
}
