use std::collections::{BTreeSet, HashMap};

use crate::core::{
    hash_segments, AnchorKind, AtomKind, BuiltAnchor, BuiltUnit, ParsedAtom, SourceKind, UnitKind,
};

pub const UNIT_BUILDER_VERSION: &str = "repo-unit-v2";
pub const MERGE_BELOW: usize = 120;
pub const MAX_TOKENS: usize = 800;

/// Session segmentation size policy (`.pi-files/session-indexing-implementation.md`).
pub const SEGMENT_MIN_TOKENS: usize = 140;
pub const SEGMENT_TARGET_TOKENS: usize = 450;

pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4).max(1)
}

fn content_atom(atom: &ParsedAtom) -> bool {
    !atom.text.trim().is_empty()
        && atom.text.chars().any(char::is_alphanumeric)
        && matches!(
            atom.kind,
            AtomKind::Paragraph
                | AtomKind::ListItem
                | AtomKind::BlockQuote
                | AtomKind::CodeBlock
                | AtomKind::Function
                | AtomKind::Class
                | AtomKind::Module
                | AtomKind::Declaration
                | AtomKind::Comment
        )
}

fn content_atom_at(atoms: &[ParsedAtom], index: usize) -> bool {
    if !content_atom(&atoms[index]) {
        return false;
    }
    if atoms[index].kind != AtomKind::Paragraph {
        return true;
    }
    let mut parent = atoms[index].parent_index;
    while let Some(parent_index) = parent {
        if matches!(
            atoms[parent_index].kind,
            AtomKind::ListItem | AtomKind::BlockQuote
        ) {
            return false;
        }
        parent = atoms[parent_index].parent_index;
    }
    true
}

fn unit_hash(kind: UnitKind, atom_hashes: &[&str], evidence: &str, routing: &str) -> String {
    let mut pieces = vec![UNIT_BUILDER_VERSION, kind.as_str()];
    pieces.extend_from_slice(atom_hashes);
    pieces.push(evidence);
    pieces.push(routing);
    hash_segments(&pieces)
}

fn mentions(text: &str) -> Vec<String> {
    fn insert(output: &mut BTreeSet<String>, value: &str) {
        let clean = value.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '/' | '_' | ':' | '.' | '-')
        });
        if !clean.is_empty() && clean.len() <= 256 {
            output.insert(clean.to_string());
        }
    }

    let mut output = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else { break };
        let value = &rest[..end];
        if !value.chars().any(char::is_whitespace) {
            insert(&mut output, value);
        }
        rest = &rest[end + 1..];
    }

    rest = text;
    while let Some(start) = rest.find("](") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find(')') else { break };
        insert(&mut output, &rest[..end]);
        rest = &rest[end + 1..];
    }

    for token in text.split_whitespace() {
        if token.contains("](") {
            continue;
        }
        let clean = token.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '/' | '_' | ':' | '.' | '-')
        });
        if clean.contains('/')
            || clean.contains("::")
            || clean.contains('_')
            || clean.ends_with(".rs")
            || clean.ends_with(".md")
        {
            insert(&mut output, clean);
        }
    }
    output.into_iter().take(32).collect()
}

fn file_anchor(locator: &str, relationship: &str) -> BuiltAnchor {
    BuiltAnchor {
        kind: AnchorKind::File,
        value: locator.to_string(),
        relationship: relationship.to_string(),
        confidence: "deterministic".to_string(),
    }
}

fn code_anchors(locator: &str, atom: &ParsedAtom) -> Vec<BuiltAnchor> {
    let mut anchors = vec![file_anchor(locator, "defined_in")];
    if let Some(symbol) = atom.metadata["symbol"]
        .as_str()
        .filter(|value| !value.is_empty())
    {
        anchors.push(BuiltAnchor {
            kind: AnchorKind::Symbol,
            value: symbol.to_string(),
            relationship: "defines".to_string(),
            confidence: "deterministic".to_string(),
        });
    }
    anchors
}

fn prose_anchors(locator: &str, evidence: &str) -> Vec<BuiltAnchor> {
    let mut anchors = vec![file_anchor(locator, "documented_in")];
    for mention in mentions(evidence).into_iter().take(16) {
        if mention.contains('/') || mention.contains('.') {
            anchors.push(BuiltAnchor {
                kind: AnchorKind::File,
                value: mention,
                relationship: "mentions".to_string(),
                confidence: "deterministic".to_string(),
            });
        } else {
            anchors.push(BuiltAnchor {
                kind: AnchorKind::Symbol,
                value: mention,
                relationship: "mentions".to_string(),
                confidence: "heuristic".to_string(),
            });
        }
    }
    anchors
}

fn prose_routing(locator: &str, breadcrumb: &str, evidence: &str) -> String {
    let found = mentions(evidence);
    format!(
        "source: document\npath: {locator}\nheading: {breadcrumb}\nmentions: {}",
        found.join(" ")
    )
}

fn code_routing(locator: &str, atom: &ParsedAtom) -> String {
    let symbol = &atom.breadcrumb;
    let signature = atom.metadata["signature"].as_str().unwrap_or_default();
    let references = atom.metadata["references"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    format!(
        "source: code\npath: {locator}\nsymbol: {symbol}\nkind: {}\nsignature: {signature}\nreferences: {references}",
        atom.kind.as_str()
    )
}

fn make_unit(
    kind: UnitKind,
    atom_indices: Vec<usize>,
    atoms: &[ParsedAtom],
    evidence: String,
    routing: String,
    anchors: Vec<BuiltAnchor>,
) -> BuiltUnit {
    let hashes: Vec<&str> = atom_indices
        .iter()
        .map(|index| atoms[*index].content_hash.as_str())
        .collect();
    let source_slices: Vec<serde_json::Value> = atom_indices
        .iter()
        .map(|index| {
            serde_json::json!({
                "atom_hash": atoms[*index].content_hash,
                "start_offset": atoms[*index].start_offset,
                "end_offset": atoms[*index].end_offset,
            })
        })
        .collect();
    BuiltUnit {
        kind,
        token_count: estimate_tokens(&evidence),
        content_hash: unit_hash(kind, &hashes, &evidence, &routing),
        evidence_text: evidence,
        routing_text: routing,
        atom_indices,
        metadata: serde_json::json!({
            "source_slices": source_slices,
        }),
        anchors,
    }
}

pub fn split_oversized(text: &str, max_chars: usize) -> Vec<(String, usize, usize)> {
    let max_chars = max_chars.max(1);
    let mut output = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let candidate_end = text[start..]
            .char_indices()
            .nth(max_chars)
            .map(|(offset, _)| start + offset)
            .unwrap_or(text.len());
        let mut end = candidate_end;
        if end < text.len() {
            if let Some((offset, character)) = text[start..end]
                .char_indices()
                .rev()
                .find(|(_, character)| matches!(character, '.' | '\n'))
            {
                end = start + offset + character.len_utf8();
            }
        }
        if end <= start {
            end = candidate_end.max(start + 1).min(text.len());
        }
        output.push((text[start..end].to_string(), start, end));
        start = end;
    }
    output
}

fn section_key(atoms: &[ParsedAtom], index: usize) -> usize {
    let mut current = index;
    loop {
        if matches!(atoms[current].kind, AtomKind::Heading | AtomKind::File) {
            return current;
        }
        match atoms[current].parent_index {
            Some(parent) if parent < current => current = parent,
            _ => return 0,
        }
    }
}

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
    atom.metadata["node_kind"]
        .as_str()
        .is_some_and(|kind| IMPORT_NODE_KINDS.contains(&kind))
}

fn leading_context_text(atom: &ParsedAtom) -> Option<&str> {
    atom.metadata["leading_context"]["text"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
}

fn comment_is_attached(atoms: &[ParsedAtom], index: usize) -> bool {
    let atom = &atoms[index];
    atoms[index + 1..]
        .iter()
        .find(|candidate| content_atom(candidate) && candidate.kind != AtomKind::Comment)
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
                && (atom.kind != AtomKind::Comment || comment_is_standalone_unit(atoms, *candidate))
        })
        .map(|(candidate, _)| candidate)
        .collect()
}

fn shell_header(atoms: &[ParsedAtom], index: usize, children: &[usize]) -> String {
    let atom = &atoms[index];
    let Some(first_child) = children.iter().map(|child| atoms[*child].start_offset).min() else {
        return String::new();
    };
    let end = first_child.saturating_sub(atom.start_offset).min(atom.text.len());
    atom.text[..end].trim_end().to_string()
}

fn prepend_leading_context(evidence: &mut String, atom: &ParsedAtom) {
    if let Some(context) = leading_context_text(atom) {
        evidence.push_str(context.trim_end());
        evidence.push_str("\n\n");
    }
}

fn whole_atom_evidence(atom: &ParsedAtom) -> String {
    let mut evidence = String::new();
    prepend_leading_context(&mut evidence, atom);
    evidence.push_str(&atom.breadcrumb);
    evidence.push_str("\n\n");
    evidence.push_str(&atom.text);
    evidence
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
    for child in children {
        let signature = atoms[*child].metadata["signature"]
            .as_str()
            .unwrap_or_default();
        let line = signature.split('{').next().unwrap_or(signature).trim();
        if line.is_empty() {
            let name = atoms[*child]
                .breadcrumb
                .rsplit(" > ")
                .next()
                .unwrap_or_default();
            evidence.push_str(name);
        } else {
            evidence.push_str(line);
        }
        evidence.push('\n');
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
    let routing = format!(
        "source: code\npath: {locator}\nsymbol: {locator} > imports\nkind: imports"
    );
    let mut unit = make_unit(
        UnitKind::Code,
        import_indices.to_vec(),
        atoms,
        evidence,
        routing,
        vec![file_anchor(locator, "defined_in")],
    );
    unit.metadata["unit_shape"] = serde_json::json!("imports");
    Some(unit)
}

fn build_code(atoms: &[ParsedAtom], locator: &str) -> Vec<BuiltUnit> {
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
        if atom.kind == AtomKind::Comment && !comment_is_standalone_unit(atoms, index) {
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
                    vec![index],
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
                vec![index],
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

fn build_prose(atoms: &[ParsedAtom], locator: &str) -> Vec<BuiltUnit> {
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut order = Vec::new();
    for (index, _) in atoms
        .iter()
        .enumerate()
        .filter(|(index, _)| content_atom_at(atoms, *index))
    {
        let key = section_key(atoms, index);
        if !groups.contains_key(&key) {
            order.push(key);
        }
        groups.entry(key).or_default().push(index);
    }

    let mut units = Vec::new();
    for key in order {
        let mut current = Vec::new();
        for index in groups.remove(&key).unwrap_or_default() {
            let breadcrumb = atoms[index].breadcrumb.clone();
            let single_evidence = format!("{breadcrumb}\n\n{}", atoms[index].text);
            if estimate_tokens(&single_evidence) > MAX_TOKENS {
                push_prose(&mut units, atoms, locator, std::mem::take(&mut current));
                let max_chars = (MAX_TOKENS * 4).saturating_sub(breadcrumb.chars().count() + 2);
                let pieces = split_oversized(&atoms[index].text, max_chars);
                let anchors = prose_anchors(locator, &single_evidence);
                for (piece, start, end) in &pieces {
                    let evidence = format!("{breadcrumb}\n\n{piece}");
                    let routing = prose_routing(locator, &breadcrumb, &evidence);
                    let mut unit = make_unit(
                        UnitKind::Prose,
                        vec![index],
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
                    units.push(unit);
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
                push_prose(&mut units, atoms, locator, std::mem::take(&mut current));
            }
            current.push(index);
        }
        push_prose(&mut units, atoms, locator, current);
    }

    let mut merged: Vec<BuiltUnit> = Vec::new();
    for unit in units {
        if let Some(previous) = merged.last_mut() {
            if unit.token_count < MERGE_BELOW
                && previous.token_count + unit.token_count <= MAX_TOKENS
                && previous.routing_text == unit.routing_text
            {
                previous.atom_indices.extend(unit.atom_indices);
                previous.evidence_text.push_str("\n\n");
                previous.evidence_text.push_str(
                    unit.evidence_text
                        .split_once("\n\n")
                        .map(|(_, body)| body)
                        .unwrap_or(&unit.evidence_text),
                );
                previous.token_count = estimate_tokens(&previous.evidence_text);
                let hashes: Vec<&str> = previous
                    .atom_indices
                    .iter()
                    .map(|index| atoms[*index].content_hash.as_str())
                    .collect();
                previous.content_hash = unit_hash(
                    UnitKind::Prose,
                    &hashes,
                    &previous.evidence_text,
                    &previous.routing_text,
                );
                let source_slices: Vec<serde_json::Value> = previous
                    .atom_indices
                    .iter()
                    .map(|index| {
                        serde_json::json!({
                            "atom_hash": atoms[*index].content_hash,
                            "start_offset": atoms[*index].start_offset,
                            "end_offset": atoms[*index].end_offset,
                        })
                    })
                    .collect();
                previous.metadata["source_slices"] = serde_json::json!(source_slices);
                let mut merged_anchor_values: Vec<String> = previous
                    .anchors
                    .iter()
                    .map(|anchor| anchor.value.clone())
                    .collect();
                for anchor in unit.anchors {
                    if !merged_anchor_values.contains(&anchor.value) {
                        merged_anchor_values.push(anchor.value.clone());
                        previous.anchors.push(anchor);
                    }
                }
                continue;
            }
        }
        merged.push(unit);
    }
    merged
}

fn push_prose(
    output: &mut Vec<BuiltUnit>,
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
    output.push(make_unit(
        UnitKind::Prose,
        indices,
        atoms,
        evidence,
        routing,
        anchors,
    ));
}

pub fn build_units(atoms: &[ParsedAtom], source_kind: SourceKind, locator: &str) -> Vec<BuiltUnit> {
    match source_kind {
        SourceKind::Code => build_code(atoms, locator),
        SourceKind::Markdown | SourceKind::Text => build_prose(atoms, locator),
        SourceKind::GitCommit | SourceKind::AgentSession => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::markdown::parse_markdown;

    #[test]
    fn markdown_units_are_reversible_and_have_routing_text() {
        let parsed = parse_markdown("# Auth\n\nRefresh tokens rotate.", "README");
        let units = build_units(&parsed.atoms, SourceKind::Markdown, "README.md");
        assert_eq!(units.len(), 1);
        assert!(!units[0].atom_indices.is_empty());
        assert!(units[0].routing_text.contains("heading: README > Auth"));
    }

    #[test]
    fn routing_projection_is_deterministic_and_keeps_backticked_symbols() {
        let parsed = parse_markdown(
            "# Auth\n\nSee [auth design](docs/auth-plan.md), then use `TokenStore` in `src/auth.rs`.",
            "README",
        );
        let first = build_units(&parsed.atoms, SourceKind::Markdown, "README.md");
        let second = build_units(&parsed.atoms, SourceKind::Markdown, "README.md");
        assert_eq!(first, second);
        assert!(first[0].routing_text.contains("TokenStore"));
        assert!(first[0].routing_text.contains("src/auth.rs"));
        assert!(first[0].routing_text.contains("docs/auth-plan.md"));
        assert!(!first[0].routing_text.contains("design]("));
    }

    #[test]
    fn code_routing_uses_the_qualified_symbol() {
        let atoms =
            crate::ingest::code::parse_code("impl Session { fn refresh(&self) {} }", "src/auth.rs")
                .unwrap();
        let units = build_units(&atoms, SourceKind::Code, "src/auth.rs");
        assert!(units.iter().any(|unit| unit
            .routing_text
            .contains("symbol: src/auth.rs > impl Session > refresh")));
        assert_eq!(atoms[0].text, "impl Session { fn refresh(&self) {} }");
    }

    #[test]
    fn markdown_link_targets_change_canonical_units() {
        let first = parse_markdown("# Links\n\nSee [auth](docs/auth.md).", "README");
        let second = parse_markdown("# Links\n\nSee [auth](docs/session.md).", "README");
        let first_units = build_units(&first.atoms, SourceKind::Markdown, "README.md");
        let second_units = build_units(&second.atoms, SourceKind::Markdown, "README.md");
        assert_ne!(first_units[0].content_hash, second_units[0].content_hash);
        assert!(first_units[0].evidence_text.contains("docs/auth.md"));
    }

    #[test]
    fn oversized_code_has_bounded_units_and_exact_ranges() {
        let long = "x".repeat(4_000);
        let source = format!("fn large() {{ let value = \"{long}\"; }}");
        let atoms = crate::ingest::code::parse_code(&source, "src/large.rs").unwrap();
        let units = build_units(&atoms, SourceKind::Code, "src/large.rs");
        assert!(units.len() > 1);
        assert!(units.iter().all(|unit| unit.token_count <= MAX_TOKENS));
        assert!(units
            .iter()
            .all(|unit| unit.metadata["source_slices"][0]["start_offset"].is_number()));
    }

    #[test]
    fn oversized_prose_splits_within_the_limit() {
        let body = "A useful sentence. ".repeat(1_000);
        let parsed = parse_markdown(&format!("# Long\n\n{body}"), "README");
        let units = build_units(&parsed.atoms, SourceKind::Markdown, "README.md");
        assert!(units.len() > 1);
        assert!(units.iter().all(|unit| unit.token_count <= MAX_TOKENS));

        let near_limit = "x".repeat(3_190);
        let parsed = parse_markdown(&format!("# Heading\n\n{near_limit}"), "README");
        let units = build_units(&parsed.atoms, SourceKind::Markdown, "README.md");
        assert!(units.iter().all(|unit| unit.token_count <= MAX_TOKENS));
    }

    #[test]
    fn containers_emit_shells_and_children_keep_their_units() {
        let source = "impl Session {\n    fn refresh(&self) { let refresh_token = 1; }\n    fn validate(&self) { let validation_code = 2; }\n}\n";
        let atoms = crate::ingest::code::parse_code(source, "src/auth.rs").unwrap();
        let units = build_units(&atoms, SourceKind::Code, "src/auth.rs");
        let shell = units
            .iter()
            .find(|unit| unit.metadata["unit_shape"] == "shell")
            .expect("container shell unit");
        assert!(shell.evidence_text.contains("fn refresh(&self)"));
        assert!(shell.evidence_text.contains("fn validate(&self)"));
        assert!(!shell.evidence_text.contains("refresh_token"));
        assert!(!shell.evidence_text.contains("validation_code"));
        assert_eq!(
            shell.metadata["elided_children"].as_array().map(Vec::len),
            Some(2)
        );
        let refresh = units
            .iter()
            .find(|unit| unit.routing_text.contains("> refresh"))
            .expect("child unit for refresh");
        assert!(refresh.evidence_text.contains("refresh_token"));
    }

    #[test]
    fn attached_docs_deduplicate_and_trivial_comments_are_skipped() {
        let source = "/// Attached doc for refresh.\nfn refresh() {}\n\n// tiny\n\n// This standalone comment explains a subtle invariant worth keeping.\nfn validate() {}\n";
        let atoms = crate::ingest::code::parse_code(source, "src/auth.rs").unwrap();
        let units = build_units(&atoms, SourceKind::Code, "src/auth.rs");
        assert!(units
            .iter()
            .all(|unit| !unit.routing_text.contains("/// Attached")));
        let refresh = units
            .iter()
            .find(|unit| unit.routing_text.contains("> refresh"))
            .expect("refresh unit");
        assert!(refresh.evidence_text.contains("Attached doc for refresh."));
        assert!(units
            .iter()
            .all(|unit| !unit.evidence_text.contains("// tiny")));
        assert!(units
            .iter()
            .any(|unit| unit.evidence_text.contains("subtle invariant")));
    }

    #[test]
    fn imports_aggregate_into_one_file_unit() {
        let source = "use std::collections::HashMap;\nuse std::fmt;\n\nfn refresh() {}\n";
        let atoms = crate::ingest::code::parse_code(source, "src/auth.rs").unwrap();
        let units = build_units(&atoms, SourceKind::Code, "src/auth.rs");
        let import_units: Vec<_> = units
            .iter()
            .filter(|unit| unit.metadata["unit_shape"] == "imports")
            .collect();
        assert_eq!(import_units.len(), 1);
        assert!(import_units[0]
            .evidence_text
            .contains("use std::collections::HashMap;"));
        assert!(import_units[0].evidence_text.contains("use std::fmt;"));
        assert_eq!(
            units
                .iter()
                .filter(|unit| unit
                    .evidence_text
                    .contains("use std::collections::HashMap;"))
                .count(),
            1
        );
    }
}
