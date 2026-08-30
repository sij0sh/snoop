use std::path::Path;

use crate::core::{hash_segments, AtomKind, BuiltAnchor, BuiltUnit, ParsedAtom, UnitKind};
use crate::ingest::units::{estimate_tokens, MAX_TOKENS};

use super::align::{
    file_change_kind, parse_hunks, reconcile_alignments, Alignment, BoundaryConfidence, ChangeKind,
    Hunk,
};
use super::anchors::{git_anchors, git_routing};
use super::history::{blob, changed_files, parent_oid, CommitRef};
use super::MAX_HUNKS_PER_FILE;

const GIT_UNIT_VERSION: &str = "git-unit-v3";
const MAX_PATCH_CHARS: usize = 256 * 1024;
const MAX_ALIGN_BYTES: usize = 10 * 1024 * 1024;

pub(super) struct PushContext<'a> {
    pub(super) output: &'a mut Vec<BuiltUnit>,
    pub(super) atoms: &'a [ParsedAtom],
    pub(super) atom_indices: Vec<usize>,
    pub(super) header: String,
    pub(super) routing: String,
    pub(super) metadata: serde_json::Value,
    pub(super) anchors: Vec<BuiltAnchor>,
}

pub(super) fn split_hunk_text(text: &str, max_tokens: usize) -> Vec<String> {
    let max_chars = max_tokens.saturating_sub(1).max(1) * 4;
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for line in text.split_inclusive('\n') {
        let cost = estimate_tokens(line);
        if !current.is_empty() && used + cost > max_tokens {
            pieces.push(std::mem::take(&mut current));
            used = 0;
        }
        if cost > max_tokens {
            let budget_chars = max_chars;
            let mut start = 0usize;
            while start < line.len() {
                let mut limit = (start + budget_chars).min(line.len());
                while limit > start && !line.is_char_boundary(limit) {
                    limit -= 1;
                }
                let mut end = line[start..limit]
                    .char_indices()
                    .next_back()
                    .map(|(offset, _)| start + offset)
                    .unwrap_or(start);
                if end <= start {
                    end = line.len();
                }
                let piece = &line[start..end];
                pieces.push(piece.to_string());
                start = end;
            }
            used = 0;
            continue;
        }
        current.push_str(line);
        used += cost;
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    if pieces.is_empty() {
        pieces.push(String::new());
    }
    pieces
}

pub(super) fn push_units(context: PushContext<'_>, hunk_texts: &[String]) {
    let PushContext {
        output,
        atoms,
        atom_indices,
        header,
        routing,
        metadata,
        anchors,
    } = context;
    let header_tokens = estimate_tokens(&header);
    let body_budget = MAX_TOKENS.saturating_sub(header_tokens + 1).max(1);
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for text in hunk_texts {
        let split: Vec<String> = if estimate_tokens(text) > body_budget {
            split_hunk_text(text, body_budget)
        } else {
            vec![text.clone()]
        };
        for piece in split {
            let cost = estimate_tokens(&piece);
            if !current.is_empty() && used + cost > body_budget {
                parts.push(std::mem::take(&mut current));
                used = 0;
            }
            current.push_str(&piece);
            used += cost;
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    let multi = parts.len() > 1;
    let hashes: Vec<&str> = atom_indices
        .iter()
        .map(|index| atoms[*index].content_hash.as_str())
        .collect();
    for (index, body) in parts.iter().enumerate() {
        let evidence = format!("{header}{body}");
        let mut unit_routing = routing.clone();
        let mut unit_metadata = metadata.clone();
        if multi {
            unit_routing.push_str(&format!("\npart: {}", index + 1));
            unit_metadata["part"] = serde_json::json!(index + 1);
        }
        output.push(BuiltUnit {
            kind: UnitKind::Git,
            token_count: estimate_tokens(&evidence),
            content_hash: hash_segments(&[
                GIT_UNIT_VERSION,
                "git",
                &hashes.join("\n"),
                &evidence,
                &unit_routing,
            ]),
            evidence_text: evidence,
            routing_text: unit_routing,
            metadata: unit_metadata,
            anchors: anchors.clone(),
        });
    }
}

pub fn ingest_commit(
    root: &Path,
    commit: &CommitRef,
) -> Result<Vec<BuiltUnit>, Box<dyn std::error::Error + Send + Sync>> {
    let files = changed_files(root, &commit.oid)?;
    let short = commit
        .oid
        .get(..8)
        .unwrap_or(commit.oid.as_str())
        .to_string();
    let subject = commit
        .message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let commit_breadcrumb = format!("git:{short}");
    let mut atoms = vec![ParsedAtom {
        kind: AtomKind::Commit,
        parent_index: None,
        ordinal: 0,
        start_offset: 0,
        end_offset: commit.message.len(),
        text: commit.message.clone(),
        content_hash: ParsedAtom::content_hash_of(
            AtomKind::Commit,
            &commit_breadcrumb,
            &commit.message,
        ),
        breadcrumb: commit_breadcrumb.clone(),
        metadata: {
            let mut metadata = serde_json::json!({
                "commit": commit.oid,
                "subject": subject,
            });
            crate::metadata::timestamp::set(&mut metadata, Some(commit.timestamp));
            metadata
        },
    }];
    let mut units = Vec::new();
    let mut ordinal = 1u32;

    for file in &files {
        let patch_text: String = if file.patch.chars().count() > MAX_PATCH_CHARS {
            file.patch.chars().take(MAX_PATCH_CHARS).collect()
        } else {
            file.patch.clone()
        };
        let file_breadcrumb = format!("{commit_breadcrumb} > {}", file.path);
        let fc_index = atoms.len();
        let fc_ordinal = ordinal;
        ordinal += 1;
        atoms.push(ParsedAtom {
            kind: AtomKind::FileChange,
            parent_index: Some(0),
            ordinal: fc_ordinal,
            start_offset: 0,
            end_offset: patch_text.len(),
            text: patch_text.clone(),
            content_hash: ParsedAtom::content_hash_of(
                AtomKind::FileChange,
                &file_breadcrumb,
                &file.patch,
            ),
            breadcrumb: file_breadcrumb.clone(),
            metadata: serde_json::json!({
                "path": file.path,
                "status": file.status.to_string(),
            }),
        });

        let hunks: Vec<Hunk> = parse_hunks(&file.patch)
            .into_iter()
            .take(MAX_HUNKS_PER_FILE)
            .collect();
        let mut hunk_indices = Vec::with_capacity(hunks.len());
        for (number, hunk) in hunks.iter().enumerate() {
            let hunk_breadcrumb = format!("{file_breadcrumb} > hunk {}", number + 1);
            let index = atoms.len();
            let hunk_ordinal = ordinal;
            ordinal += 1;
            atoms.push(ParsedAtom {
                kind: AtomKind::DiffHunk,
                parent_index: Some(fc_index),
                ordinal: hunk_ordinal,
                start_offset: hunk.start_offset,
                end_offset: hunk.end_offset,
                text: hunk.text.clone(),
                content_hash: ParsedAtom::content_hash_of(
                    AtomKind::DiffHunk,
                    &hunk_breadcrumb,
                    &hunk.text,
                ),
                breadcrumb: hunk_breadcrumb,
                metadata: serde_json::json!({
                    "path": file.path,
                    "old_start": hunk.old_start,
                    "old_count": hunk.old_count,
                    "new_start": hunk.new_start,
                    "new_count": hunk.new_count,
                }),
            });
            hunk_indices.push(index);
        }

        let file_change = file_change_kind(file.status);
        let old_path = file.old_path.clone().unwrap_or_else(|| file.path.clone());
        let supported = crate::ingest::code::supports_code_path(&file.path)
            || crate::ingest::code::supports_code_path(&old_path);
        let language = crate::ingest::code::language_name(&file.path)
            .or_else(|| crate::ingest::code::language_name(&old_path));
        let parent = parent_oid(root, &commit.oid);
        let aligned: Vec<Alignment> = if hunks.is_empty() {
            vec![Alignment {
                old_span: None,
                new_span: None,
                hunk_indices: Vec::new(),
                change_kind: if file.old_path.is_some() {
                    ChangeKind::Moved
                } else {
                    file_change
                },
                strategy: "file",
                confidence: BoundaryConfidence::Medium,
            }]
        } else if supported {
            let before = parent
                .as_deref()
                .and_then(|revision| blob(root, revision, &old_path))
                .filter(|content| content.len() <= MAX_ALIGN_BYTES)
                .unwrap_or_default();
            let after = if file.status == 'D' {
                String::new()
            } else {
                blob(root, &commit.oid, &file.path)
                    .filter(|content| content.len() <= MAX_ALIGN_BYTES)
                    .unwrap_or_default()
            };
            if before.is_empty() && after.is_empty() {
                vec![Alignment {
                    old_span: None,
                    new_span: None,
                    hunk_indices: (0..hunks.len()).collect(),
                    change_kind: file_change,
                    strategy: "file",
                    confidence: BoundaryConfidence::Medium,
                }]
            } else {
                reconcile_alignments(
                    &hunks,
                    &before,
                    &after,
                    &old_path,
                    &file.path,
                    file.old_path.is_some(),
                    file_change,
                )
            }
        } else {
            vec![Alignment {
                old_span: None,
                new_span: None,
                hunk_indices: (0..hunks.len()).collect(),
                change_kind: file_change,
                strategy: "file",
                confidence: BoundaryConfidence::Medium,
            }]
        };

        let mut base_metadata = serde_json::json!({
            "commit": commit.oid,
            "path": file.path,
        });
        crate::metadata::timestamp::set(&mut base_metadata, Some(commit.timestamp));
        if let Some(old) = &file.old_path {
            base_metadata["old_path"] = serde_json::json!(old);
        }
        for alignment in &aligned {
            let display = alignment.new_span.as_ref().or(alignment.old_span.as_ref());
            let (header, routing, mut metadata) = match display {
                Some(span) => (
                    format!("commit {short} {subject}\n\n{}\n\n", span.breadcrumb),
                    git_routing(&short, &subject, &file.path, Some(&span.name)),
                    {
                        let mut metadata = base_metadata.clone();
                        metadata["strategy"] = serde_json::json!(alignment.strategy);
                        crate::metadata::code_symbol::set_symbol(&mut metadata, &span.name);
                        metadata["symbol_id"] = serde_json::json!(span.breadcrumb);
                        metadata["declaration_kind"] = serde_json::json!(span.kind.as_str());
                        metadata
                    },
                ),
                None => (
                    format!("commit {short} {subject}\n\n{}\n\n", file.path),
                    git_routing(&short, &subject, &file.path, None),
                    {
                        let mut metadata = base_metadata.clone();
                        metadata["strategy"] = serde_json::json!(alignment.strategy);
                        metadata
                    },
                ),
            };
            metadata["change_kind"] = serde_json::json!(alignment.change_kind.as_str());
            metadata["boundary_confidence"] = serde_json::json!(alignment.confidence.as_str());
            metadata["hunks"] = serde_json::json!(alignment
                .hunk_indices
                .iter()
                .map(|index| format!("hunk {}", index + 1))
                .collect::<Vec<_>>());
            let evolution_symbol = matches!(
                alignment.change_kind,
                ChangeKind::Renamed | ChangeKind::Deleted
            );
            if evolution_symbol {
                if let Some(old_span) = &alignment.old_span {
                    metadata["old_symbol"] = serde_json::json!(old_span.breadcrumb);
                }
            }
            if let Some(language) = language {
                metadata["language"] = serde_json::json!(language);
            }
            let old_symbol_anchor = if evolution_symbol {
                alignment
                    .old_span
                    .as_ref()
                    .map(|span| span.breadcrumb.as_str())
            } else {
                None
            };
            let new_symbol_anchor = if matches!(alignment.change_kind, ChangeKind::Deleted) {
                None
            } else {
                display.map(|span| span.name.as_str())
            };
            let anchors = git_anchors(
                &commit.oid,
                file.old_path.as_deref(),
                &file.path,
                old_symbol_anchor,
                new_symbol_anchor,
            );
            let texts: Vec<String> = if alignment.hunk_indices.is_empty() {
                vec![patch_text.clone()]
            } else {
                alignment
                    .hunk_indices
                    .iter()
                    .map(|index| hunks[*index].text.clone())
                    .collect()
            };
            let atom_indices: Vec<usize> = alignment
                .hunk_indices
                .iter()
                .filter_map(|index| hunk_indices.get(*index).copied())
                .collect();
            push_units(
                PushContext {
                    output: &mut units,
                    atoms: &atoms,
                    atom_indices,
                    header,
                    routing,
                    metadata,
                    anchors,
                },
                &texts,
            );
        }
    }
    Ok(units)
}
