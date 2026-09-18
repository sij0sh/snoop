//! Anchor expansion: seeds, candidate scoring, and selection ordering.

use std::collections::{HashMap, HashSet};

use super::ExpansionDebug;
use crate::core::SelectionReason;
use crate::store::Store;

pub(crate) const EXPANSION_SEEDS: usize = 5;
pub(crate) const EXPANSION_MAX_UNITS: usize = 6;
const EXPANSION_CANDIDATES_PER_ANCHOR: usize = 10;
const ANCHOR_BONUS: f64 = 0.02;
const EXACT_SYMBOL_BONUS: f64 = 0.03;
const SOURCE_DIVERSITY_BONUS: f64 = 0.01;
/// Lifecycle companions per memory seed: a replacement plus one side of a
/// dispute is enough context; the graph is bounded, not explored.
const HINDSIGHT_LIFECYCLE_PER_SEED: usize = 2;
/// Provenance neighbors per memory seed: at most one supporting
/// current-source unit and one rationale/history/session unit, so a
/// mature record with a long evidence tail cannot flood the packet.
const HINDSIGHT_PROVENANCE_PER_SEED: usize = 2;

pub(crate) type SelectionEntry = (i64, f64, Option<u32>, Option<Vec<SelectionReason>>);

pub(crate) struct ExpansionPlan {
    pub(crate) selection_order: Vec<SelectionEntry>,
    pub(crate) debug: Vec<ExpansionDebug>,
}

/// A candidate survives lifecycle filtering when it is not curated memory
/// or its visibility tier is unlocked for this query.
fn lifecycle_eligible(
    store: &Store,
    candidate: i64,
    hindsight_allowed: &[&str],
) -> rusqlite::Result<bool> {
    let Some(unit) = store.unit_by_id(candidate)? else {
        return Ok(false);
    };
    if unit.source_kind != crate::core::SourceKind::HindsightMemory {
        return Ok(true);
    }
    Ok(hindsight_allowed.contains(&crate::metadata::hindsight::visibility(&unit.metadata)))
}

pub(crate) fn plan_expansion(
    store: &Store,
    fused: &[(i64, f64, u32)],
    query_text: &str,
    diagnostics: bool,
    excluded: &HashSet<i64>,
    hindsight_allowed: &[&str],
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
    // Per-seed budgets for Hindsight seeds: lifecycle companions and
    // provenance neighbors are each bounded so one mature record cannot
    // dominate expansion.
    let mut lifecycle_used: HashMap<i64, usize> = HashMap::new();
    let mut provenance_used: HashMap<i64, usize> = HashMap::new();
    for &seed in &seed_ids {
        let seed_is_memory =
            seed_kinds.get(&seed) == Some(&crate::core::SourceKind::HindsightMemory);
        for anchor in store.anchors_for_unit(seed)? {
            let kind = anchor.kind.as_str();
            let value = anchor.value.as_str();
            let is_lifecycle_link =
                seed_is_memory && kind == "memory" && anchor.relationship != "self";
            if seed_is_memory && !is_lifecycle_link && kind != "memory" {
                // Provenance/support links from a memory seed: bounded.
                let used = provenance_used.entry(seed).or_insert(0);
                if *used >= HINDSIGHT_PROVENANCE_PER_SEED {
                    continue;
                }
                *used += 1;
            }
            if is_lifecycle_link {
                let used = lifecycle_used.entry(seed).or_insert(0);
                if *used >= HINDSIGHT_LIFECYCLE_PER_SEED {
                    continue;
                }
                *used += 1;
            }
            // Budget-bounded by design: EXPANSION_CANDIDATES_PER_ANCHOR is a
            // ranking budget, not a display promise, so the truncation count
            // is intentionally ignored here (defect-audit c6 pins this).
            let (connected, _more) =
                store.units_for_anchor(kind, value, EXPANSION_CANDIDATES_PER_ANCHOR)?;
            for candidate in connected {
                if candidate == seed || excluded.contains(&candidate) {
                    continue;
                }
                // Expansion never resurrects lifecycle-ineligible memories:
                // a superseded record joins only a history query, a
                // disputed claim only a conflict/review query.
                if !lifecycle_eligible(store, candidate, hindsight_allowed)? {
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
                if diagnostics {
                    let reason = if is_lifecycle_link {
                        SelectionReason::HindsightLifecycleExpansion(format!(
                            "{}:{}",
                            anchor.relationship, value
                        ))
                    } else {
                        SelectionReason::AnchorExpansion(kind.to_string(), value.to_string(), seed)
                    };
                    candidate_reasons.entry(candidate).or_default().push(reason);
                    expansion_debug.push(ExpansionDebug {
                        seed_unit: seed,
                        anchor_kind: kind.to_string(),
                        anchor_value: value.to_string(),
                        candidate,
                        expanded_score: score,
                        accepted: false,
                    });
                }
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
    if diagnostics {
        for debug in expansion_debug.iter_mut() {
            debug.accepted = accepted.contains(&debug.candidate)
                && selection_order
                    .iter()
                    .any(|(id, _, _, _)| *id == debug.candidate);
        }
    }
    Ok(ExpansionPlan {
        selection_order,
        debug: expansion_debug,
    })
}
