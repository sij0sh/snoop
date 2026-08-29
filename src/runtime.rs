use std::collections::{HashMap, HashSet};

use crate::core::{ContextItem, ContextPacket, RepoId, SelectionReason};
use crate::inference::Embedder;
use crate::store::{cosine, Store};

pub const RRF_K: u64 = 60;

const NEAR_DUP_THRESHOLD: f32 = 0.985;
const EXPANSION_SEEDS: usize = 5;
const EXPANSION_CANDIDATES_PER_ANCHOR: usize = 10;
const ANCHOR_BONUS: f64 = 0.02;
const EXACT_SYMBOL_BONUS: f64 = 0.03;
const SOURCE_DIVERSITY_BONUS: f64 = 0.01;
const EXPANSION_MAX_UNITS: usize = 6;
const ROLE_POOL: usize = 30;

#[derive(Debug, Clone, Copy)]
pub struct QueryChannels {
    evidence_lexical: bool,
    evidence_vector: bool,
    routing_lexical: bool,
    routing_vector: bool,
}

impl QueryChannels {
    pub const fn evidence_only() -> Self {
        Self {
            evidence_lexical: true,
            evidence_vector: true,
            routing_lexical: false,
            routing_vector: false,
        }
    }

    pub const fn evidence_lexical_only() -> Self {
        Self {
            evidence_lexical: true,
            evidence_vector: false,
            routing_lexical: false,
            routing_vector: false,
        }
    }

    pub fn for_embedder(embedder: Option<&dyn crate::inference::Embedder>) -> Self {
        match embedder {
            Some(_) => Self {
                evidence_lexical: true,
                evidence_vector: true,
                routing_lexical: true,
                routing_vector: true,
            },
            None => Self {
                evidence_lexical: true,
                evidence_vector: false,
                routing_lexical: true,
                routing_vector: false,
            },
        }
    }

}

#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub channels: QueryChannels,
    pub top_n: usize,
    pub max_tokens: usize,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            channels: QueryChannels::for_embedder(None),
            top_n: 25,
            max_tokens: 6_000,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DebugReport {
    pub evidence_lexical: Vec<(i64, f64)>,
    pub evidence_vector: Vec<(i64, f32)>,
    pub routing_lexical: Vec<(i64, f64)>,
    pub routing_vector: Vec<(i64, f32)>,
    pub fused: Vec<(i64, f64, u32)>,
    pub expansion: Vec<ExpansionDebug>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ExpansionDebug {
    pub seed_unit: i64,
    pub anchor_kind: String,
    pub anchor_value: String,
    pub candidate: i64,
    pub expanded_score: f64,
    pub accepted: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryReport {
    pub packet: ContextPacket,
    pub debug: DebugReport,
}

pub fn rrf_fuse(channels: &[Vec<i64>], k: u64) -> Vec<(i64, f64, u32)> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    for channel in channels {
        let mut seen = HashSet::new();
        for (position, id) in channel.iter().enumerate() {
            if seen.insert(*id) {
                *scores.entry(*id).or_default() += 1.0 / (k + position as u64 + 1) as f64;
            }
        }
    }
    let mut ranked: Vec<(i64, f64, u32)> = scores
        .into_iter()
        .map(|(id, score)| (id, score, 0))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    for (index, item) in ranked.iter_mut().enumerate() {
        item.2 = index as u32 + 1;
    }
    ranked
}

fn rank_of(channel: &[i64], id: i64) -> Option<u32> {
    channel
        .iter()
        .position(|candidate| *candidate == id)
        .map(|position| position as u32 + 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Facet {
    Rationale,
    Evolution,
    Validation,
    PriorWork,
    Conflict,
    Invariant,
    CurrentBehavior,
}

fn query_tokens(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect()
}

fn detect_facets(query: &str) -> Vec<Facet> {
    let tokens = query_tokens(query);
    let has_token = |words: &[&str]| {
        words
            .iter()
            .any(|word| tokens.iter().any(|token| token == word))
    };
    let has_phrase = |phrases: &[&str]| {
        phrases.iter().any(|phrase| {
            let phrase_tokens = query_tokens(phrase);
            tokens
                .windows(phrase_tokens.len())
                .any(|window| window == phrase_tokens.as_slice())
        })
    };
    let mut facets = Vec::new();
    if has_token(&["why", "rationale"]) || has_phrase(&["what is the reason", "reasoning behind"]) {
        facets.push(Facet::Rationale);
    }
    if has_token(&[
        "when",
        "introduced",
        "renamed",
        "history",
        "originally",
        "previously",
        "legacy",
        "retired",
        "deprecated",
    ]) {
        facets.push(Facet::Evolution);
    }
    if has_token(&[
        "test",
        "tests",
        "tested",
        "pass",
        "passed",
        "passes",
        "invoked",
        "validated",
        "validation",
    ]) {
        facets.push(Facet::Validation);
    }
    if has_token(&[
        "prior",
        "previous",
        "attempt",
        "attempts",
        "fix",
        "fixes",
        "fixed",
        "investigated",
        "investigation",
    ]) {
        facets.push(Facet::PriorWork);
    }
    if has_token(&[
        "conflict",
        "conflicts",
        "contradict",
        "contradicts",
        "contradicted",
        "versus",
    ]) || has_phrase(&["which fix", "instead of"])
    {
        facets.push(Facet::Conflict);
    }
    if has_token(&[
        "invariant",
        "invariants",
        "across",
        "consistent",
        "consistently",
    ]) || has_phrase(&["same rule"])
    {
        facets.push(Facet::Invariant);
    }
    if has_token(&["current", "currently", "how", "now"]) {
        facets.push(Facet::CurrentBehavior);
    }
    if facets.is_empty() {
        facets.push(Facet::CurrentBehavior);
    }
    facets
}

fn role_of_kind(kind: crate::core::SourceKind) -> &'static str {
    match kind {
        crate::core::SourceKind::Code => "current_truth",
        crate::core::SourceKind::Markdown | crate::core::SourceKind::Text => "design_rationale",
        crate::core::SourceKind::GitCommit => "change_origin",
        crate::core::SourceKind::AgentSession => "prior_work",
    }
}

fn preferred_role(facet: Facet) -> &'static str {
    match facet {
        Facet::CurrentBehavior => "current_truth",
        Facet::Rationale => "design_rationale",
        Facet::Evolution => "change_origin",
        Facet::PriorWork => "prior_work",
        Facet::Validation => "prior_work",
        Facet::Conflict => "prior_work",
        Facet::Invariant => "current_truth",
    }
}

/// Oversampled candidate pool for the role-aware builders.
pub fn query(
    store: &Store,
    repo_id: RepoId,
    embedder: Option<&dyn Embedder>,
    text: &str,
    options: &QueryOptions,
) -> Result<QueryReport, Box<dyn std::error::Error + Send + Sync>> {
    if (options.channels.evidence_vector || options.channels.routing_vector) && embedder.is_none() {
        return Err("vector channels require a configured embedder".into());
    }
    let evidence_lexical = if options.channels.evidence_lexical {
        store.fts_search(
            repo_id,
            "evidence_text",
            text,
            options.top_n,
        )?
    } else {
        Vec::new()
    };
    let routing_lexical = if options.channels.routing_lexical {
        store.fts_search(
            repo_id,
            "routing_text",
            text,
            options.top_n,
        )?
    } else {
        Vec::new()
    };
    let query_vector = if options.channels.evidence_vector || options.channels.routing_vector {
        Some(embedder.unwrap().embed_query(text)?)
    } else {
        None
    };
    let evidence_vector = if options.channels.evidence_vector {
        store.top_k_cosine(
            repo_id,
            "evidence",
            embedder.unwrap().model_version(),
            query_vector.as_deref().unwrap_or_default(),
            options.top_n,
        )?
    } else {
        Vec::new()
    };
    let routing_vector = if options.channels.routing_vector {
        store.top_k_cosine(
            repo_id,
            "routing",
            embedder.unwrap().model_version(),
            query_vector.as_deref().unwrap_or_default(),
            options.top_n,
        )?
    } else {
        Vec::new()
    };

    let evidence_lexical_ids: Vec<i64> = evidence_lexical.iter().map(|item| item.0).collect();
    let evidence_vector_ids: Vec<i64> = evidence_vector.iter().map(|item| item.0).collect();
    let routing_lexical_ids: Vec<i64> = routing_lexical.iter().map(|item| item.0).collect();
    let routing_vector_ids: Vec<i64> = routing_vector.iter().map(|item| item.0).collect();
    let mut enabled = Vec::new();
    if options.channels.evidence_lexical {
        enabled.push(evidence_lexical_ids.clone());
    }
    if options.channels.evidence_vector {
        enabled.push(evidence_vector_ids.clone());
    }
    if options.channels.routing_lexical {
        enabled.push(routing_lexical_ids.clone());
    }
    if options.channels.routing_vector {
        enabled.push(routing_vector_ids.clone());
    }
    let fused = rrf_fuse(&enabled, RRF_K);

    let mut items = Vec::new();
    let mut expansion_debug: Vec<ExpansionDebug> = Vec::new();

    let mut seen_hashes: HashSet<String> = HashSet::new();

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
    let query_terms: HashSet<String> = text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| term.to_lowercase())
        .collect();
    let mut candidate_scores: HashMap<i64, f64> = HashMap::new();
    let mut candidate_reasons: HashMap<i64, Vec<SelectionReason>> = HashMap::new();
    for &seed in &seed_ids {
        for (kind, _relationship, anchor_id) in store.anchors_for_unit(seed)? {
            let Some(value) = store.anchor_value(repo_id, &kind, anchor_id)? else {
                continue;
            };
            let connected = store.units_for_anchor(
                repo_id,
                &kind,
                &value,
                EXPANSION_CANDIDATES_PER_ANCHOR,
            )?;
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

    type SelectionEntry = (i64, f64, Option<u32>, Option<Vec<SelectionReason>>);
    let mut selection_order: Vec<SelectionEntry> = Vec::new();
    let mut placed: HashSet<i64> = HashSet::new();
    for (id, score, rank) in &fused {
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

    let mut accepted_ids: Vec<i64> = Vec::new();
    let mut role_assignments: HashMap<i64, (String, bool)> = HashMap::new();

        let pool: Vec<(i64, u32)> = selection_order
            .iter()
            .take(ROLE_POOL)
            .map(|(id, _, rank, _)| (*id, rank.unwrap_or(u32::MAX)))
            .collect();
        let facets = detect_facets(text);
        let mut pool_with_kinds: Vec<(i64, u32, crate::core::SourceKind)> = Vec::new();
        for (id, rank) in &pool {
            if let Some(unit) = store.unit_by_id(*id)? {
                pool_with_kinds.push((*id, *rank, unit.source_kind));
            }
        }
        let mut required_roles: Vec<&'static str> =
            facets.iter().map(|facet| preferred_role(*facet)).collect();
        required_roles.dedup();

        let mut role_vectors: HashMap<&'static str, Vec<Vec<f32>>> = HashMap::new();
        let mut admitted: Vec<i64> = Vec::new();
        #[allow(clippy::too_many_arguments)]
        fn admit(
            store: &Store,
            embedder: Option<&dyn Embedder>,
            options: &QueryOptions,
            id: i64,
            role: &'static str,
            required: bool,
            role_vectors: &mut HashMap<&'static str, Vec<Vec<f32>>>,
            admitted: &mut Vec<i64>,
            role_assignments: &mut HashMap<i64, (String, bool)>,
            seen_hashes: &mut HashSet<String>,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            if admitted.contains(&id) {
                return Ok(false);
            }
            let Some(unit) = store.unit_by_id(id)? else {
                return Ok(false);
            };
            if !seen_hashes.insert(unit.content_hash.clone()) {
                return Ok(false);
            }
            if options.channels.evidence_vector {
                let Some(embedder) = embedder else {
                    return Ok(false);
                };
                let vector = store.get_vector(id, "evidence", embedder.model_version())?;
                if vector.as_ref().is_some_and(|candidate| {
                    role_vectors.get(role).is_some_and(|kept| {
                        kept.iter()
                            .any(|v| cosine(candidate, v) > NEAR_DUP_THRESHOLD)
                    })
                }) {
                    return Ok(false);
                }
                if let Some(vector) = vector {
                    role_vectors.entry(role).or_default().push(vector);
                }
            }
            role_assignments.insert(id, (role.to_string(), required));
            admitted.push(id);
            Ok(true)
        }

        for role in &required_roles {
            let candidates: Vec<i64> = pool_with_kinds
                .iter()
                .filter(|(_, _, kind)| role_of_kind(*kind) == *role)
                .map(|(id, _, _)| *id)
                .collect();
            for id in candidates {
                if admit(
                    store,
                    embedder,
                    options,
                    id,
                    role,
                    true,
                    &mut role_vectors,
                    &mut admitted,
                    &mut role_assignments,
                    &mut seen_hashes,
                )? {
                    break;
                }
            }
        }

        let supporting_roles: Vec<&'static str> = [
            "current_truth",
            "design_rationale",
            "change_origin",
            "prior_work",
        ]
        .into_iter()
        .filter(|role| !required_roles.contains(role))
        .collect();
        for role in supporting_roles {
            for (id, _, kind) in &pool_with_kinds {
                if role_of_kind(*kind) != role {
                    continue;
                }
                if admit(
                    store,
                    embedder,
                    options,
                    *id,
                    role,
                    false,
                    &mut role_vectors,
                    &mut admitted,
                    &mut role_assignments,
                    &mut seen_hashes,
                )? {
                    break;
                }
            }
        }

        let mut fill_order = pool_with_kinds.clone();
        fill_order.sort_by_key(|(_, rank, _)| *rank);
        for (id, _, kind) in fill_order {
            if admitted.contains(&id) {
                continue;
            }
            admit(
                store,
                embedder,
                options,
                id,
                role_of_kind(kind),
                false,
                &mut role_vectors,
                &mut admitted,
                &mut role_assignments,
                &mut seen_hashes,
            )?;
        }
        accepted_ids.extend(admitted);

    let used = accepted_ids
        .iter()
        .filter_map(|id| store.unit_by_id(*id).ok().flatten())
        .map(|unit| unit.token_count)
        .sum();

    let reason_map: HashMap<i64, (Option<u32>, Option<Vec<SelectionReason>>)> = selection_order
        .iter()
        .map(|(id, _, rank, reasons)| (*id, (*rank, reasons.clone())))
        .collect();

    for unit_id in &accepted_ids {
        let (fused_rank, expansion_reasons) =
            reason_map.get(unit_id).cloned().unwrap_or((None, None));
        let Some(unit) = store.unit_by_id(*unit_id)? else {
            continue;
        };
        let mut reasons = Vec::new();
        if let Some(rank) = rank_of(&evidence_lexical_ids, *unit_id) {
            reasons.push(SelectionReason::EvidenceLexicalRank(rank));
        }
        if let Some(rank) = rank_of(&evidence_vector_ids, *unit_id) {
            reasons.push(SelectionReason::EvidenceVectorRank(rank));
        }
        if let Some(rank) = rank_of(&routing_lexical_ids, *unit_id) {
            reasons.push(SelectionReason::RoutingLexicalRank(rank));
        }
        if let Some(rank) = rank_of(&routing_vector_ids, *unit_id) {
            reasons.push(SelectionReason::RoutingVectorRank(rank));
        }
        if let Some(rank) = fused_rank {
            reasons.push(SelectionReason::RrfRank(rank));
        }
        if let Some(expansion_reasons) = expansion_reasons {
            reasons.append(&mut expansion_reasons.clone());
        }
        if let Some((role, required)) = role_assignments.get(unit_id) {
            reasons.push(SelectionReason::RoleAware(role.clone(), *required));
        }
        let anchors = store
            .anchors_for_unit(*unit_id)?
            .into_iter()
            .filter_map(|(kind, relationship, anchor_id)| {
                store
                    .anchor_value(repo_id, &kind, anchor_id)
                    .transpose()
                    .map(|value| value.map(|value| format!("{kind}:{value}:{relationship}")))
            })
            .collect::<rusqlite::Result<Vec<String>>>()?;
        items.push(ContextItem {
            unit_id: unit.id,
            source_kind: unit.source_kind,
            evidence_text: unit.evidence_text,
            source_locator: unit.locator,
            atom_ids: unit.atom_ids,
            source_slices: unit.metadata["source_slices"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            anchors,
            timestamp: unit.timestamp,
            selected_because: reasons,
        });
    }

    fn group_of(kind: crate::core::SourceKind) -> u8 {
        match kind {
            crate::core::SourceKind::Code => 0,
            crate::core::SourceKind::Markdown | crate::core::SourceKind::Text => 1,
            crate::core::SourceKind::GitCommit => 2,
            crate::core::SourceKind::AgentSession => 3,
        }
    }
    let mut indexed: Vec<(usize, ContextItem)> = items.into_iter().enumerate().collect();
    indexed.sort_by(|a, b| {
        group_of(a.1.source_kind)
            .cmp(&group_of(b.1.source_kind))
            .then(a.0.cmp(&b.0))
    });
    let items: Vec<ContextItem> = indexed.into_iter().map(|(_, item)| item).collect();

    Ok(QueryReport {
        packet: ContextPacket {
            query: text.to_string(),
            items,
            token_count: used,
            budget: options.max_tokens,
        },
        debug: DebugReport {
            evidence_lexical,
            evidence_vector,
            routing_lexical,
            routing_vector,
            fused,
            expansion: expansion_debug,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hash_segments;
    use crate::core::{BuiltAnchor, BuiltUnit, UnitKind};
    use crate::ingest::units::estimate_tokens;
    use crate::store::SourceIngest;

    #[test]
    fn rrf_prefers_agreement_and_is_deterministic() {
        let result = rrf_fuse(&[vec![1, 2, 3], vec![2, 1, 4]], RRF_K);
        assert_eq!(result[0].0, 1);
        assert_eq!(result[1].0, 2);
        assert_eq!(
            result.iter().map(|item| item.2).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn facet_detection_matches_exact_tokens_and_phrases() {
        let facets = detect_facets("how does token refresh validate before rotation");
        assert!(facets.contains(&Facet::CurrentBehavior));
        assert!(
            !facets.contains(&Facet::PriorWork),
            "ordering language in a current-behavior query must not open the history lane"
        );
        assert!(
            !facets.contains(&Facet::Conflict),
            "substring `or` inside `before` must not open the conflict facet"
        );

        let facets = detect_facets("was the fix validation ordering or the mutex");
        assert!(
            !facets.contains(&Facet::Conflict),
            "the standalone token `or` opens no facet: {facets:?}"
        );
        assert!(facets.contains(&Facet::PriorWork));

        assert!(detect_facets("mutex instead of validation ordering").contains(&Facet::Conflict));
        assert!(detect_facets("which fix landed for the guard").contains(&Facet::Conflict));
        assert!(detect_facets("guard versus mutex approaches").contains(&Facet::Conflict));
        assert!(!detect_facets("which fixture covers the guard").contains(&Facet::Conflict));
        assert!(!detect_facets("use this instead often").contains(&Facet::Conflict));

        let facets = detect_facets("single-use token invariant across auth and v2 components");
        assert!(
            facets.contains(&Facet::Invariant),
            "invariant vocabulary opens the invariant facet: {facets:?}"
        );
        assert!(
            detect_facets("is the rule consistent across components").contains(&Facet::Invariant)
        );
        assert!(detect_facets("same rule in both files").contains(&Facet::Invariant));
        assert!(
            detect_facets("what did the legacy v1 refresh flow do").contains(&Facet::Evolution),
            "legacy vocabulary opens evolution"
        );
        assert!(detect_facets("was that module retired").contains(&Facet::Evolution));

        let facets = detect_facets("did the auth tests pass or were they only invoked");
        assert!(facets.contains(&Facet::Validation));
        assert!(
            !facets.contains(&Facet::Conflict),
            "the standalone token `or` opens no facet: {facets:?}"
        );

        assert_eq!(
            detect_facets("retry policy backoff delay"),
            vec![Facet::CurrentBehavior]
        );
    }

    fn code_unit(evidence: &str, file: &str, anchor_symbol: Option<&str>) -> BuiltUnit {
        let mut anchors = vec![BuiltAnchor {
            kind: crate::core::AnchorKind::File,
            value: file.to_string(),
            relationship: "defines".to_string(),
            confidence: "deterministic".to_string(),
        }];
        if let Some(symbol) = anchor_symbol {
            anchors.push(BuiltAnchor {
                kind: crate::core::AnchorKind::Symbol,
                value: symbol.to_string(),
                relationship: "defines".to_string(),
                confidence: "deterministic".to_string(),
            });
        }
        BuiltUnit {
            kind: UnitKind::Code,
            evidence_text: evidence.to_string(),
            routing_text: String::new(),
            token_count: estimate_tokens(evidence),
            content_hash: hash_segments(&[evidence]),
            atom_indices: Vec::new(),
            metadata: serde_json::json!({}),
            anchors,
        }
    }

    fn commit_units(
        store: &mut Store,
        repo: RepoId,
        locator: &str,
        units: &[BuiltUnit],
    ) -> Vec<i64> {
        store
            .commit_source(SourceIngest {
                repo_id: repo,
                kind: crate::core::SourceKind::Code,
                locator,
                content_hash: &hash_segments(&[locator, &units.len().to_string()]),
                modified_at: None,
                metadata: serde_json::json!({}),
                atoms: &[],
                units,
            })
            .unwrap();
        store
            .units_for_source(repo, locator)
            .unwrap()
            .iter()
            .map(|unit| unit.id.0)
            .collect()
    }

    fn options_all() -> QueryOptions {
        QueryOptions {
            // Lexical channels only: with vectors on, a tiny fixture would put
            // every unit in the vector top-n and hide the provenance path.
            channels: QueryChannels::for_embedder(None),
            top_n: 25,
            max_tokens: 6_000,
        }
    }

    #[test]
    fn query_channels_follow_embedder_availability() {
        let embedder = crate::inference::MockEmbedder::new("mock-v1");
        let hybrid = QueryChannels::for_embedder(Some(&embedder));
        assert!(hybrid.evidence_lexical && hybrid.evidence_vector);
        assert!(hybrid.routing_lexical && hybrid.routing_vector);

        let lexical = QueryChannels::for_embedder(None);
        assert!(lexical.evidence_lexical && lexical.routing_lexical);
        assert!(!lexical.evidence_vector);
        assert!(!lexical.routing_vector);
    }

    #[test]
    fn vector_channels_require_a_configured_embedder() {
        let store = Store::open_in_memory().unwrap();
        let options = QueryOptions {
            channels: QueryChannels::for_embedder(Some(&crate::inference::MockEmbedder::new("mock-v1"))),
            ..options_all()
        };
        let error = query(&store, RepoId(1), None, "alpha", &options).unwrap_err();
        assert!(error.to_string().contains("configured embedder"));
    }

    #[test]
    fn lexical_mode_runs_without_an_embedder_and_dedups_by_content_hash() {
        let mut store = Store::open_in_memory().unwrap();
        let repo = RepoId(1);
        store.ensure_repository("/repo").unwrap();
        let original = commit_units(
            &mut store,
            repo,
            "src/a.rs",
            &[code_unit(
                "alpha text about auth",
                "src/a.rs",
                Some("alpha_fn"),
            )],
        );

        let duplicate = commit_units(
            &mut store,
            repo,
            "src/dup.rs",
            &[code_unit(
                "alpha text about auth",
                "src/dup.rs",
                Some("alpha_fn"),
            )],
        );
        let options = QueryOptions {
            channels: QueryChannels::for_embedder(None),
            ..options_all()
        };
        let report = query(&store, repo, None, "alpha", &options).unwrap();
        let ids: Vec<i64> = report
            .packet
            .items
            .iter()
            .map(|item| item.unit_id.0)
            .collect();
        assert!(ids.contains(&original[0]), "seed unit admitted: {ids:?}");
        assert!(
            !ids.contains(&duplicate[0]),
            "identical content is deduplicated without vectors: {ids:?}"
        );
        assert!(
            report.debug.evidence_vector.is_empty() && report.debug.routing_vector.is_empty(),
            "lexical mode runs no vector channels"
        );
    }
}
