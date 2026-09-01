use super::*;

use crate::core::hash_segments;
use crate::core::{BuiltAnchor, BuiltUnit, UnitKind};
use crate::ingest::units::estimate_tokens;
use crate::runtime::facets::Facet;
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
    assert!(detect_facets("is the rule consistent across components").contains(&Facet::Invariant));
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
    }];
    if let Some(symbol) = anchor_symbol {
        anchors.push(BuiltAnchor {
            kind: crate::core::AnchorKind::Symbol,
            value: symbol.to_string(),
            relationship: "defines".to_string(),
        });
    }
    BuiltUnit {
        kind: UnitKind::Code,
        evidence_text: evidence.to_string(),
        routing_text: String::new(),
        token_count: estimate_tokens(evidence),
        content_hash: hash_segments(&[evidence]),
        metadata: serde_json::json!({}),
        anchors,
    }
}

fn commit_units(store: &mut Store, locator: &str, units: &[BuiltUnit]) -> Vec<i64> {
    store
        .commit_source(SourceIngest {
            kind: crate::core::SourceKind::Code,
            locator,
            content_hash: &hash_segments(&[locator, &units.len().to_string()]),
            modified_at: None,
            metadata: serde_json::json!({}),
            units,
        })
        .unwrap();
    store
        .units_for_source(locator)
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
        // Debug fields are inspected by these tests.
        diagnostics: true,
        ..QueryOptions::default()
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
        channels: QueryChannels::for_embedder(Some(&crate::inference::MockEmbedder::new(
            "mock-v1",
        ))),
        ..options_all()
    };
    let error = query(&store, None, "alpha", &options).unwrap_err();
    assert!(error.to_string().contains("configured embedder"));
}

#[test]
fn lexical_mode_runs_without_an_embedder_and_dedups_by_content_hash() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    let original = commit_units(
        &mut store,
        "src/a.rs",
        &[code_unit(
            "alpha text about auth",
            "src/a.rs",
            Some("alpha_fn"),
        )],
    );

    let duplicate = commit_units(
        &mut store,
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
    let report = query(&store, None, "alpha", &options).unwrap();
    let ids: Vec<i64> = report
        .debug
        .as_ref()
        .unwrap()
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
        report.debug.as_ref().unwrap().evidence_vector.is_empty()
            && report.debug.as_ref().unwrap().routing_vector.is_empty(),
        "lexical mode runs no vector channels"
    );
}

/// Evidence padded to exactly `tokens` estimated tokens (tokens >= 2).
/// estimate_tokens counts ceil(chars / 4); "auth " plus the filler hits it.
fn sized_evidence(tokens: usize) -> String {
    format!("auth {}", "w".repeat(tokens * 4 - 5))
}

#[test]
fn budget_skips_oversized_candidate_and_admits_smaller_ones() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    let ids = commit_units(
        &mut store,
        "src/budget.rs",
        &[
            code_unit(&sized_evidence(10), "src/budget.rs", None),
            code_unit(&sized_evidence(2), "src/budget.rs", None),
            code_unit(&sized_evidence(3), "src/budget.rs", None),
        ],
    );
    let options = QueryOptions {
        max_tokens: 5,
        ..options_all()
    };
    let report = query(&store, None, "auth", &options).unwrap();
    let packet_ids: Vec<i64> = report
        .debug
        .as_ref()
        .unwrap()
        .items
        .iter()
        .map(|item| item.unit_id.0)
        .collect();
    assert!(
        !packet_ids.contains(&ids[0]),
        "the 10-token unit exceeds the 5-token budget: {packet_ids:?}"
    );
    assert_eq!(
        report.packet.token_count, report.packet.budget,
        "admission must fill the budget exactly"
    );
    assert!(
        packet_ids.contains(&ids[1]) && packet_ids.contains(&ids[2]),
        "smaller lower-ranked units fill the budget: {packet_ids:?}"
    );
}

#[test]
fn budget_rejects_every_candidate_that_does_not_fit() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    commit_units(
        &mut store,
        "src/huge.rs",
        &[code_unit(&sized_evidence(10), "src/huge.rs", None)],
    );
    let options = QueryOptions {
        max_tokens: 5,
        ..options_all()
    };
    let report = query(&store, None, "auth", &options).unwrap();
    assert!(report.packet.items.is_empty());
    assert_eq!(report.packet.token_count, 0);
}

#[test]
fn required_role_admission_respects_the_budget() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    let ids = commit_units(
        &mut store,
        "src/required.rs",
        &[
            code_unit(&sized_evidence(10), "src/required.rs", None),
            code_unit(&sized_evidence(2), "src/required.rs", None),
        ],
    );
    let options = QueryOptions {
        max_tokens: 5,
        ..options_all()
    };
    // "how does ... work" opens the current-behavior facet whose required
    // role maps onto code units.
    let report = query(&store, None, "how does the auth login flow work", &options).unwrap();
    let packet_ids: Vec<i64> = report
        .debug
        .as_ref()
        .unwrap()
        .items
        .iter()
        .map(|item| item.unit_id.0)
        .collect();
    assert!(
        !packet_ids.contains(&ids[0]),
        "required roles must not bypass the budget: {packet_ids:?}"
    );
    assert!(
        packet_ids.contains(&ids[1]),
        "the next required-role candidate is admitted after the skip: {packet_ids:?}"
    );
    assert!(report.packet.token_count <= report.packet.budget);
}

#[test]
fn packet_timestamps_are_rendered_against_injected_now() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    let built = code_unit("alpha timestamp", "src/time.rs", None);
    store
        .commit_source(SourceIngest {
            kind: crate::core::SourceKind::Code,
            locator: "src/time.rs",
            content_hash: "time-source",
            modified_at: Some(1_700_000_000),
            metadata: serde_json::json!({}),
            units: &[built],
        })
        .unwrap();
    let options = QueryOptions {
        now: 1_700_003_600,
        ..options_all()
    };
    let report = query(&store, None, "alpha", &options).unwrap();
    assert_eq!(report.packet.items[0].timestamp.as_deref(), Some("1h ago"));
    assert_eq!(
        report.debug.unwrap().items[0].timestamp,
        Some(1_700_000_000)
    );
}

#[test]
fn exclusion_removes_channel_hits_and_backfills_the_limit() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    let ids = commit_units(
        &mut store,
        "src/all.rs",
        &[
            code_unit("alpha one", "src/all.rs", None),
            code_unit("alpha two", "src/all.rs", None),
            code_unit("alpha three", "src/all.rs", None),
        ],
    );
    let options = QueryOptions {
        top_n: 2,
        max_per_source: usize::MAX,
        exclude_unit_ids: HashSet::from([ids[0]]),
        ..options_all()
    };
    let report = query(&store, None, "alpha", &options).unwrap();
    let debug = report.debug.unwrap();
    assert_eq!(debug.evidence_lexical.len(), 2);
    assert!(!debug.fused.iter().any(|(id, _, _)| *id == ids[0]));
    assert_eq!(report.packet.items.len(), 2);
}

#[test]
fn required_role_pick_is_not_blocked_by_source_cap() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    commit_units(
        &mut store,
        "src/required.rs",
        &[code_unit("auth implementation", "src/required.rs", None)],
    );
    let options = QueryOptions {
        max_per_source: 0,
        ..options_all()
    };
    let report = query(&store, None, "how does auth work", &options).unwrap();
    assert_eq!(report.packet.items.len(), 1);
}

#[test]
fn admission_caps_each_source_and_orders_items_by_fused_rank() {
    let mut store = Store::open_in_memory().unwrap();
    store.bind_repository("/repo").unwrap();
    let flood_units: Vec<BuiltUnit> = (0..15)
        .map(|index| code_unit(&format!("alpha flood {index}"), "src/flood.rs", None))
        .collect();
    commit_units(&mut store, "src/flood.rs", &flood_units);
    commit_units(
        &mut store,
        "src/other.rs",
        &[
            code_unit("alpha other one", "src/other.rs", None),
            code_unit("alpha other two", "src/other.rs", None),
        ],
    );

    let report = query(&store, None, "alpha", &options_all()).unwrap();
    let flood_count = report
        .packet
        .items
        .iter()
        .filter(|item| item.source_locator == "src/flood.rs")
        .count();
    assert_eq!(flood_count, 3);
    assert!(report
        .packet
        .items
        .iter()
        .any(|item| item.source_locator == "src/other.rs"));

    let ranks: Vec<u32> = report
        .debug
        .unwrap()
        .items
        .iter()
        .filter_map(|item| {
            item.selected_because
                .iter()
                .find_map(|reason| match reason {
                    SelectionReason::RrfRank(rank) => Some(*rank),
                    _ => None,
                })
        })
        .collect();
    assert!(ranks.windows(2).all(|pair| pair[0] <= pair[1]), "{ranks:?}");
}
