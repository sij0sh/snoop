use std::collections::{HashMap, HashSet};

use crate::core::{
    ContextItem, ContextPacket, ItemDiagnostics, RetrievalUnit, SelectionReason, UnitId,
};
use crate::inference::Embedder;
use crate::store::Store;

pub const RRF_K: u64 = 60;

const NEAR_DUP_THRESHOLD: f32 = 0.985;
const ROLE_POOL: usize = 30;

mod admit;
mod expansion;
mod locale;
mod relevance;
mod facets;
mod options;

use expansion::plan_expansion;
use facets::{detect_facets, hindsight_visibility, preferred_roles, role_of_kind};
pub use options::{HindsightVisibility, QueryChannels, QueryOptions};
#[derive(Debug, Clone, serde::Serialize)]
pub struct DebugReport {
    pub evidence_lexical: Vec<(i64, f64)>,
    pub evidence_vector: Vec<(i64, f32)>,
    pub routing_lexical: Vec<(i64, f64)>,
    pub routing_vector: Vec<(i64, f32)>,
    pub fused: Vec<(i64, f64, u32)>,
    pub expansion: Vec<ExpansionDebug>,
    pub items: Vec<ItemDiagnostics>,
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
    pub debug: Option<DebugReport>,
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

/// Load a unit once per query; ranking, admission, and rendering share it.
fn cached_unit(
    store: &Store,
    cache: &mut HashMap<i64, Option<RetrievalUnit>>,
    id: i64,
) -> Result<Option<RetrievalUnit>, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(cached) = cache.get(&id) {
        return Ok(cached.clone());
    }
    let unit = store.unit_by_id(id)?;
    cache.insert(id, unit.clone());
    Ok(unit)
}

pub fn query(
    store: &Store,
    embedder: Option<&dyn Embedder>,
    text: &str,
    options: &QueryOptions,
) -> Result<QueryReport, Box<dyn std::error::Error + Send + Sync>> {
    query_with_vector(store, embedder, text, options, None)
}

/// Like [`query`], but reuses a caller-supplied query embedding when vector
/// channels are enabled. The embedder still backs every other embed path;
/// `None` behaves exactly like `query`.
pub fn query_with_vector(
    store: &Store,
    embedder: Option<&dyn Embedder>,
    text: &str,
    options: &QueryOptions,
    query_vector: Option<Vec<f32>>,
) -> Result<QueryReport, Box<dyn std::error::Error + Send + Sync>> {
    if (options.channels.evidence_vector || options.channels.routing_vector) && embedder.is_none() {
        return Err("vector channels require a configured embedder".into());
    }
    // Facets first: the Hindsight lifecycle filter must operate at channel
    // candidate selection, before any top-N truncation.
    let early_facets = detect_facets(text);
    let visibility = hindsight_visibility(&early_facets);
    let allowed_tiers = visibility.allowed_tiers();
    let channel_limit = options.top_n.saturating_add(options.exclude_unit_ids.len());
    let mut evidence_lexical = if options.channels.evidence_lexical {
        store.fts_search_hindsight("evidence_text", text, channel_limit, allowed_tiers)?
    } else {
        Vec::new()
    };
    let mut routing_lexical = if options.channels.routing_lexical {
        store.fts_search_hindsight("routing_text", text, channel_limit, allowed_tiers)?
    } else {
        Vec::new()
    };
    let query_vector = if options.channels.evidence_vector || options.channels.routing_vector {
        match query_vector {
            Some(vector) => Some(vector),
            None => Some(embedder.unwrap().embed_query(text)?),
        }
    } else {
        None
    };
    let mut evidence_vector = if options.channels.evidence_vector {
        store.top_k_cosine_hindsight(
            "evidence",
            embedder.unwrap().model_version(),
            query_vector.as_deref().unwrap_or_default(),
            channel_limit,
            allowed_tiers,
        )?
    } else {
        Vec::new()
    };
    let mut routing_vector = if options.channels.routing_vector {
        store.top_k_cosine_hindsight(
            "routing",
            embedder.unwrap().model_version(),
            query_vector.as_deref().unwrap_or_default(),
            channel_limit,
            allowed_tiers,
        )?
    } else {
        Vec::new()
    };

    for channel in [&mut evidence_lexical, &mut routing_lexical] {
        channel.retain(|(id, _)| !options.exclude_unit_ids.contains(id));
        channel.truncate(options.top_n);
    }
    for channel in [&mut evidence_vector, &mut routing_vector] {
        channel.retain(|(id, _)| !options.exclude_unit_ids.contains(id));
        channel.truncate(options.top_n);
    }

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
    let mut fused = rrf_fuse(&enabled, RRF_K);

    // Hindsight confidence prior: a modest rerank inside the currently
    // eligible lifecycle class, after the mandatory status filter and
    // before expansion. Status always dominates confidence — a superseded
    // authoritative record never outranks an active strong one for an
    // ordinary query, because the former never reaches this point.
    let mut unit_cache: HashMap<i64, Option<RetrievalUnit>> = HashMap::new();
    let mut hindsight_reasons: HashMap<i64, Vec<SelectionReason>> = HashMap::new();
    const SCOPE_MATCH_BONUS: f64 = 0.01;
    const SCAR_REMOVAL_REVIEW_PENALTY: f64 = 0.95;
    let query_lower = text.to_lowercase();
    for entry in fused.iter_mut() {
        let Some(unit) = cached_unit(store, &mut unit_cache, entry.0)? else {
            continue;
        };
        if unit.source_kind != crate::core::SourceKind::HindsightMemory {
            continue;
        }
        let metadata = &unit.metadata;
        let confidence = crate::metadata::hindsight::confidence(metadata).unwrap_or_default();
        let status = crate::metadata::hindsight::status(metadata).unwrap_or_default();
        let mut multiplier = crate::ingest::hindsight::confidence_multiplier(&confidence);
        if crate::metadata::hindsight::kind(metadata).as_deref() == Some("scar")
            && crate::metadata::hindsight::scar_state(metadata).as_deref()
                == Some("candidate_for_removal")
        {
            // Still default-retrievable: removal signals request
            // inspection, not retirement. A small penalty only.
            multiplier *= SCAR_REMOVAL_REVIEW_PENALTY;
        }
        entry.1 *= multiplier;
        let mut reasons = vec![
            SelectionReason::HindsightStatus(status),
            SelectionReason::HindsightConfidence(confidence, multiplier),
        ];
        for path in crate::metadata::hindsight::scope_paths(metadata) {
            let stem = path.trim_end_matches("/**").trim_end_matches("/*");
            if stem.len() >= 3 && stem.contains('/') && query_lower.contains(&stem.to_lowercase()) {
                entry.1 += SCOPE_MATCH_BONUS;
                reasons.push(SelectionReason::HindsightScopeMatch(stem.to_string()));
                break;
            }
        }
        hindsight_reasons.insert(entry.0, reasons);
    }
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    for (index, item) in fused.iter_mut().enumerate() {
        item.2 = index as u32 + 1;
    }

    let mut items = Vec::new();
    let mut item_diagnostics: Vec<ItemDiagnostics> = Vec::new();
    let mut seen_hashes: HashSet<String> = HashSet::new();

    let expansion = plan_expansion(
        store,
        &fused,
        text,
        options.diagnostics,
        &options.exclude_unit_ids,
        allowed_tiers,
    )?;
    let selection_order = expansion.selection_order;
    let expansion_debug = expansion.debug;

    let mut accepted_ids: Vec<i64> = Vec::new();
    let mut role_assignments: HashMap<i64, (String, bool)> = HashMap::new();

    let pool: Vec<(i64, u32)> = selection_order
        .iter()
        .take(ROLE_POOL)
        .map(|(id, _, rank, _)| (*id, rank.unwrap_or(u32::MAX)))
        .collect();
    let mut pool_with_kinds: Vec<(i64, u32, crate::core::SourceKind)> = Vec::new();
    for (id, rank) in &pool {
        if let Some(unit) = cached_unit(store, &mut unit_cache, *id)? {
            pool_with_kinds.push((*id, *rank, unit.source_kind));
        }
    }
    // Translated sibling sources collapse to one representative per family
    // for this query; chunks from the chosen source stay eligible.
    let suppressed = locale::suppress_locale_siblings(&pool_with_kinds, &unit_cache, text);
    if !suppressed.is_empty() {
        pool_with_kinds.retain(|(id, _, _)| !suppressed.contains(id));
    }
    // Facets were detected before candidate selection for the lifecycle
    // filter; reuse them here for multi-role admission.
    let mut required_roles: Vec<&'static str> = early_facets
        .iter()
        .flat_map(|facet| preferred_roles(*facet).iter().copied())
        .collect();
    required_roles.dedup();
    // Supporting lanes and fill admit only credible candidates; required
    // lanes stay permissive, so recall rests on that path.
    let concepts = relevance::query_concepts(text);
    let expanded_ids: HashSet<i64> = selection_order
        .iter()
        .filter(|(_, _, _, reasons)| reasons.is_some())
        .map(|(id, _, _, _)| *id)
        .collect();

    let mut role_vectors: HashMap<&'static str, Vec<Vec<f32>>> = HashMap::new();
    let mut admitted: Vec<i64> = Vec::new();
    let mut admitted_per_source: HashMap<i64, usize> = HashMap::new();
    let mut admitted_git_siblings: HashMap<i64, Vec<admit::SiblingKey>> = HashMap::new();
    let mut used_tokens: usize = 0;
    let mut hindsight_admitted: usize = 0;

    for role in &required_roles {
        let candidates: Vec<i64> = pool_with_kinds
            .iter()
            .filter(|(_, _, kind)| role_of_kind(*kind) == *role)
            .map(|(id, _, _)| *id)
            .collect();
        for id in candidates {
            if admit::admit(
                store,
                embedder,
                options,
                id,
                role,
                true,
                &mut role_vectors,
                &mut admitted,
                &mut admitted_per_source,
                &mut admitted_git_siblings,
                &mut role_assignments,
                &mut seen_hashes,
                &mut used_tokens,
                &mut unit_cache,
                &mut hindsight_admitted,
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
        "curated_memory",
    ]
    .into_iter()
    .filter(|role| !required_roles.contains(role))
    .collect();
    for role in supporting_roles {
        for (id, _, kind) in &pool_with_kinds {
            if role_of_kind(*kind) != role {
                continue;
            }
            let Some(unit) = cached_unit(store, &mut unit_cache, *id)? else {
                continue;
            };
            if !relevance::credible(&unit, &concepts, expanded_ids.contains(id)) {
                continue;
            }
            if admit::admit(
                store,
                embedder,
                options,
                *id,
                role,
                false,
                &mut role_vectors,
                &mut admitted,
                &mut admitted_per_source,
                &mut admitted_git_siblings,
                &mut role_assignments,
                &mut seen_hashes,
                &mut used_tokens,
                &mut unit_cache,
                &mut hindsight_admitted,
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
        // Fill gating applies to history only. Commit floods were the
        // observed failure; code and docs keep rank-ordered admission
        // because thin lexical matches there are cheap recall insurance
        // (A5: gating all kinds dropped task-critical routing.py).
        if kind == crate::core::SourceKind::GitCommit {
            let Some(unit) = cached_unit(store, &mut unit_cache, id)? else {
                continue;
            };
            if !relevance::credible(&unit, &concepts, expanded_ids.contains(&id)) {
                continue;
            }
        }
        admit::admit(
            store,
            embedder,
            options,
            id,
            role_of_kind(kind),
            false,
            &mut role_vectors,
            &mut admitted,
            &mut admitted_per_source,
            &mut admitted_git_siblings,
            &mut role_assignments,
            &mut seen_hashes,
            &mut used_tokens,
            &mut unit_cache,
            &mut hindsight_admitted,
        )?;
    }
    accepted_ids.extend(admitted);
    let selection_rank: HashMap<i64, usize> = selection_order
        .iter()
        .enumerate()
        .map(|(rank, (id, _, _, _))| (*id, rank))
        .collect();
    accepted_ids.sort_by_key(|id| selection_rank.get(id).copied().unwrap_or(usize::MAX));

    let reason_map: HashMap<i64, (Option<u32>, Option<Vec<SelectionReason>>)> =
        if options.diagnostics {
            selection_order
                .iter()
                .map(|(id, _, rank, reasons)| (*id, (*rank, reasons.clone())))
                .collect()
        } else {
            HashMap::new()
        };

    for unit_id in &accepted_ids {
        let Some(unit) = cached_unit(store, &mut unit_cache, *unit_id)? else {
            continue;
        };
        items.push(ContextItem {
            source_kind: unit.source_kind,
            evidence_text: unit.evidence_text.clone(),
            source_locator: unit.locator.clone(),
            timestamp: unit
                .timestamp
                .map(|timestamp| crate::metadata::timestamp::render(timestamp, options.now)),
        });
        if !options.diagnostics {
            continue;
        }
        let (fused_rank, expansion_reasons) =
            reason_map.get(unit_id).cloned().unwrap_or((None, None));
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
        if let Some(extra) = hindsight_reasons.get(unit_id) {
            reasons.extend(extra.iter().cloned());
        }
        if let Some((role, required)) = role_assignments.get(unit_id) {
            reasons.push(SelectionReason::RoleAware(role.clone(), *required));
        }
        item_diagnostics.push(ItemDiagnostics {
            unit_id: UnitId(*unit_id),
            source_slices: crate::metadata::source_slices::read(&unit.metadata),
            anchors: store.anchors_for_unit(*unit_id)?,
            selected_because: reasons,
            timestamp: unit.timestamp,
        });
    }

    Ok(QueryReport {
        packet: ContextPacket {
            query: text.to_string(),
            items,
            token_count: used_tokens,
            budget: options.max_tokens,
        },
        debug: if options.diagnostics {
            Some(DebugReport {
                evidence_lexical,
                evidence_vector,
                routing_lexical,
                routing_vector,
                fused,
                expansion: expansion_debug,
                items: item_diagnostics,
            })
        } else {
            None
        },
    })
}

#[cfg(test)]
mod regression;
#[cfg(test)]
mod tests;
