use std::collections::{HashMap, HashSet};

use crate::core::{ContextItem, ContextPacket, RepoId, SelectionReason};
use crate::inference::Embedder;
use crate::store::{cosine, Store};

pub const RRF_K: u64 = 60;

const NEAR_DUP_THRESHOLD: f32 = 0.985;
const ROLE_POOL: usize = 30;

mod expansion;
mod facets;

use expansion::plan_expansion;
use facets::{detect_facets, preferred_role, role_of_kind};

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
        store.fts_search(repo_id, "evidence_text", text, options.top_n)?
    } else {
        Vec::new()
    };
    let routing_lexical = if options.channels.routing_lexical {
        store.fts_search(repo_id, "routing_text", text, options.top_n)?
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
    let mut seen_hashes: HashSet<String> = HashSet::new();

    let expansion = plan_expansion(store, repo_id, &fused, text)?;
    let selection_order = expansion.selection_order;
    let expansion_debug = expansion.debug;

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
    let mut used_tokens: usize = 0;
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
        used_tokens: &mut usize,
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
        *used_tokens += unit.token_count;
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
                &mut used_tokens,
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
                &mut used_tokens,
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
            &mut used_tokens,
        )?;
    }
    accepted_ids.extend(admitted);

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
            token_count: used_tokens,
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
mod tests;
