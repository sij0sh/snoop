//! Admission: budget, diversity, and duplicate guards for one candidate.

use std::collections::{HashMap, HashSet};

use crate::core::{RetrievalUnit, SourceKind};
use crate::inference::Embedder;
use crate::store::{cosine, Store};

use super::cached_unit;
use super::options::QueryOptions;
use super::NEAR_DUP_THRESHOLD;

/// Identity of an admitted git sibling: path, symbol, hunk part. A second
/// unit from one commit is admitted only when it differs from admitted
/// siblings on at least one component.
pub(crate) type SiblingKey = (Option<String>, Option<String>, Option<String>);

pub(crate) fn sibling_key(unit: &RetrievalUnit) -> SiblingKey {
    let field = |name: &str| {
        unit.metadata
            .get(name)
            .and_then(|value| value.as_str())
            .map(str::to_string)
    };
    (
        field("path"),
        field("symbol_id").or_else(|| field("symbol")),
        unit.metadata.get("part").map(|value| value.to_string()),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn admit(
    store: &Store,
    embedder: Option<&dyn Embedder>,
    options: &QueryOptions,
    id: i64,
    role: &'static str,
    required: bool,
    role_vectors: &mut HashMap<&'static str, Vec<Vec<f32>>>,
    admitted: &mut Vec<i64>,
    admitted_per_source: &mut HashMap<i64, usize>,
    admitted_git_siblings: &mut HashMap<i64, Vec<SiblingKey>>,
    role_assignments: &mut HashMap<i64, (String, bool)>,
    seen_hashes: &mut HashSet<String>,
    used_tokens: &mut usize,
    unit_cache: &mut HashMap<i64, Option<RetrievalUnit>>,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    if admitted.contains(&id) {
        return Ok(false);
    }
    let Some(unit) = cached_unit(store, unit_cache, id)? else {
        return Ok(false);
    };
    if !required
        && admitted_per_source
            .get(&unit.source_id.0)
            .copied()
            .unwrap_or_default()
            >= options.max_per_source
    {
        return Ok(false);
    }
    // Per-commit sibling cap: one commit renders at most
    // `max_units_per_git_commit` items, and a second sibling only when it
    // covers a different path, symbol, or hunk part. Holds for required
    // lanes too; distinct commits stay unlimited.
    let key = if unit.source_kind == SourceKind::GitCommit {
        let key = sibling_key(&unit);
        let siblings = admitted_git_siblings
            .entry(unit.source_id.0)
            .or_default();
        if siblings.len() >= options.max_units_per_git_commit {
            return Ok(false);
        }
        if key.0.is_some() && !siblings.is_empty() && siblings.iter().all(|prior| *prior == key) {
            return Ok(false);
        }
        Some(key)
    } else {
        None
    };
    if !seen_hashes.insert(unit.content_hash.clone()) {
        return Ok(false);
    }
    // Evidence budget: a candidate that does not fit is skipped so a
    // later smaller candidate can still be admitted.
    if unit.token_count > options.max_tokens.saturating_sub(*used_tokens) {
        return Ok(false);
    }
    if options.channels.evidence_vector {
        let Some(embedder) = embedder else {
            return Ok(false);
        };
        let vector = store.get_vector(id, "evidence", embedder.model_version())?;
        if vector.as_ref().is_some_and(|candidate| {
            role_vectors.get(role).is_some_and(|kept| {
                kept
                    .iter()
                    .any(|v| cosine(candidate, v) > NEAR_DUP_THRESHOLD)
            })
        }) {
            return Ok(false);
        }
        if let Some(vector) = vector {
            role_vectors.entry(role).or_default().push(vector);
        }
    }
    if options.diagnostics {
        role_assignments.insert(id, (role.to_string(), required));
    }
    admitted.push(id);
    if let Some(key) = key {
        admitted_git_siblings
            .entry(unit.source_id.0)
            .or_default()
            .push(key);
    }
    *admitted_per_source.entry(unit.source_id.0).or_default() += 1;
    *used_tokens += unit.token_count;
    Ok(true)
}
