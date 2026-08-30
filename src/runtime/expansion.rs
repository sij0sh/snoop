//! Anchor expansion: seeds, candidate scoring, and selection ordering.

use std::collections::{HashMap, HashSet};

use super::ExpansionDebug;
use crate::core::{RepoId, SelectionReason};
use crate::store::Store;

pub(crate) const EXPANSION_SEEDS: usize = 5;
pub(crate) const EXPANSION_MAX_UNITS: usize = 6;
const EXPANSION_CANDIDATES_PER_ANCHOR: usize = 10;
const ANCHOR_BONUS: f64 = 0.02;
const EXACT_SYMBOL_BONUS: f64 = 0.03;
const SOURCE_DIVERSITY_BONUS: f64 = 0.01;

pub(crate) type SelectionEntry = (i64, f64, Option<u32>, Option<Vec<SelectionReason>>);

pub(crate) struct ExpansionPlan {
    pub(crate) selection_order: Vec<SelectionEntry>,
    pub(crate) debug: Vec<ExpansionDebug>,
}

pub(crate) fn plan_expansion(
    store: &Store,
    repo_id: RepoId,
    fused: &[(i64, f64, u32)],
    query_text: &str,
) -> Result<ExpansionPlan, Box<dyn std::error::Error + Send + Sync>> {
    let seed_ids: Vec<i64> = fused
        .iter()
        .take(EXPANSION_SEEDS)
        .map(|(unit_id, _, _)| *unit_id)
        .collect();
    let mut seed_kinds: HashMap<i64, crate::core::SourceKind> = HashMap::new();
    for &seed in &seed_ids {
        if let Some(unit) = store.unit_by_id(seed)? {
            seed_kinds.insert(seed, unit.source_kind);
        }
    }
    let query_terms: HashSet<String> = query_text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| term.to_lowercase())
        .collect();
    let mut candidate_scores: HashMap<i64, f64> = HashMap::new();
    let mut candidate_reasons: HashMap<i64, Vec<SelectionReason>> = HashMap::new();
    let mut expansion_debug: Vec<ExpansionDebug> = Vec::new();
    for &seed in &seed_ids {
        for (kind, _relationship, anchor_id) in store.anchors_for_unit(seed)? {
            let Some(value) = store.anchor_value(repo_id, &kind, anchor_id)? else {
                continue;
            };
            let connected =
                store.units_for_anchor(repo_id, &kind, &value, EXPANSION_CANDIDATES_PER_ANCHOR)?;
            for candidate in connected {
                if candidate == seed {
                    continue;
                }
                let base = candidate_scores.entry(candidate).or_insert_with(|| {
                    fused
                        .iter()
                        .find(|(id, _, _)| *id == candidate)
                        .map(|(_, score, _)| *score)
                        .unwrap_or(0.0)
                });
                let mut score = *base + ANCHOR_BONUS;
                if kind == "symbol" {
                    let exact = value
                        .split(|separator: char| !separator.is_alphanumeric())
                        .filter(|segment| !segment.is_empty() && segment.len() > 2)
                        .any(|segment| query_terms.contains(&segment.to_lowercase()));
                    if exact {
                        score += EXACT_SYMBOL_BONUS;
                    }
                }
                let diverse = store
                    .unit_by_id(candidate)?
                    .map(|unit| {
                        seed_kinds
                            .values()
                            .all(|seed_kind| *seed_kind != unit.source_kind)
                    })
                    .unwrap_or(false);
                if diverse {
                    score += SOURCE_DIVERSITY_BONUS;
                }
                candidate_scores.insert(candidate, score);
                candidate_reasons.entry(candidate).or_default().push(
                    SelectionReason::AnchorExpansion(kind.clone(), value.clone(), seed),
                );
                expansion_debug.push(ExpansionDebug {
                    seed_unit: seed,
                    anchor_kind: kind.clone(),
                    anchor_value: value.clone(),
                    candidate,
                    expanded_score: score,
                    accepted: false,
                });
            }
        }
    }
    let mut ranked_expansions: Vec<(i64, f64)> = candidate_scores.into_iter().collect();
    ranked_expansions.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    let mut accepted: HashSet<i64> = HashSet::new();
    let mut expanded_scores: HashMap<i64, f64> = HashMap::new();
    let mut expanded_reasons: HashMap<i64, Vec<SelectionReason>> = HashMap::new();
    for (candidate, score) in ranked_expansions.into_iter().take(EXPANSION_MAX_UNITS) {
        accepted.insert(candidate);
        expanded_scores.insert(candidate, score);
        expanded_reasons.insert(
            candidate,
            candidate_reasons.remove(&candidate).unwrap_or_default(),
        );
    }

    let mut selection_order: Vec<SelectionEntry> = Vec::new();
    let mut placed: HashSet<i64> = HashSet::new();
    for (id, score, rank) in fused {
        let expansion_reasons = expanded_reasons.remove(id);
        selection_order.push((*id, *score, Some(*rank), expansion_reasons));
        placed.insert(*id);
    }
    let mut pure_expansions: Vec<(i64, f64, Vec<SelectionReason>)> = expanded_scores
        .iter()
        .filter(|(id, _)| placed.insert(**id))
        .map(|(id, score)| (*id, *score, expanded_reasons.remove(id).unwrap_or_default()))
        .collect();
    pure_expansions.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    for (id, score, reasons) in pure_expansions {
        selection_order.push((id, score, None, Some(reasons)));
    }
    for debug in expansion_debug.iter_mut() {
        debug.accepted = accepted.contains(&debug.candidate)
            && selection_order
                .iter()
                .any(|(id, _, _, _)| *id == debug.candidate);
    }
    Ok(ExpansionPlan {
        selection_order,
        debug: expansion_debug,
    })
}
