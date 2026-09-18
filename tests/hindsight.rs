//! Snoop × Hindsight integration: artifact policy, lifecycle filtering,
//! confidence priors, family caps, and companion expansion.

use snoop::core::SourceKind;
use snoop::ingest::index_repository_bounded;
use snoop::runtime::{query, QueryChannels, QueryOptions};
use snoop::store::Store;

fn mem_id(n: u32) -> String {
    format!("mem_{n:032x}")
}

fn record(
    n: u32,
    kind: &str,
    status: &str,
    confidence: &str,
    statement: &str,
    extra: serde_json::Value,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "id": mem_id(n),
        "revision": 1,
        "kind": kind,
        "domains": ["architecture"],
        "statement": statement,
        "scope": {"global": false, "paths": ["src/billing/**"], "symbols": [], "concepts": ["billing"]},
        "exposeTo": ["ARCHITECTURE.md"],
        "status": status,
        "confidence": confidence,
        "basis": "policy",
        "createdAt": "2026-09-18T00:00:00Z",
        "lastValidatedAt": "2026-09-18T00:00:00Z",
        "provenance": [{"type": "source", "ref": "file:src/billing/webhook.js", "hash": "h", "classification": "observed", "reportPath": "r", "at": "t", "start": 0, "end": 1}],
        "supersedes": [],
        "contradicts": [],
    });
    for (key, val) in extra.as_object().unwrap() {
        value[key] = val.clone();
    }
    value
}

fn ledger(records: &[serde_json::Value]) -> String {
    serde_json::json!({
        "version": 1,
        "revision": records.len(),
        "records": records,
        "events": [],
        "imports": [],
        "projections": {},
    })
    .to_string()
}

fn fixture(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("src/billing")).unwrap();
    std::fs::write(
        root.join("src/billing/webhook.js"),
        "const processedEvents = new Set();\n\
         export function handleWebhook(event) {\n\
         \tif (processedEvents.has(event.id)) return;\n\
         \tprocessedEvents.add(event.id);\n\
         }\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "# Billing\n").unwrap();

    let old = mem_id(2);
    let disputed = mem_id(5);
    let records = vec![
        record(
            1,
            "constraint",
            "active",
            "authoritative",
            "Webhook processing must be idempotent quokka.",
            serde_json::json!({"domains": ["architecture", "operations", "delivery"], "exposeTo": ["ARCHITECTURE.md", "OPERATIONS.md", "DELIVERY.md"]}),
        ),
        record(
            2,
            "constraint",
            "superseded",
            "authoritative",
            "All payments settle through Stripe zedonk.",
            serde_json::json!({}),
        ),
        record(
            3,
            "constraint",
            "active",
            "supported",
            "Enterprise invoices may settle manually zedonk.",
            serde_json::json!({"supersedes": [old]}),
        ),
        record(
            4,
            "convention",
            "unverified",
            "inferred",
            "Legacy cache layout mirrors production wombat.",
            serde_json::json!({"basis": "inferred"}),
        ),
        record(
            5,
            "constraint",
            "conflicted",
            "supported",
            "Stripe owns settlement exclusively zedonk.",
            serde_json::json!({}),
        ),
        record(
            6,
            "conflict",
            "active",
            "conflicted",
            "Payment settlement ownership is disputed zedonk.",
            serde_json::json!({"conflict": {"targets": [disputed], "reason": "Enterprise manual settlement conflicts with the Stripe-only ownership rule."}, "basis": "inferred"}),
        ),
        record(
            7,
            "scar",
            "active",
            "strong",
            "Legacy authentication middleware remains because mobile clients still depend on v1 token semantics giraffe.",
            serde_json::json!({"reason": "legacy clients", "constraint": "Do not add new authentication behavior here.", "removalCondition": "Remove after v1 clients are no longer supported.", "scarState": "candidate_for_removal", "removalSignals": []}),
        ),
        record(
            8,
            "conflict",
            "resolved",
            "conflicted",
            "Old cache ownership dispute resolved xylophone.",
            serde_json::json!({"conflict": {"targets": [], "reason": "Settled by the cache charter."}, "basis": "inferred"}),
        ),
    ];
    std::fs::create_dir_all(root.join(".agents/curation/migration")).unwrap();
    std::fs::write(root.join(".agents/curation/memory.json"), ledger(&records)).unwrap();

    // Generated projections and archived imports repeat ledger language.
    // All of it must lose to the canonical record, not join it.
    std::fs::create_dir_all(root.join(".agents/engineering")).unwrap();
    for view in ["ARCHITECTURE.md", "OPERATIONS.md"] {
        std::fs::write(
            root.join(".agents/engineering").join(view),
            "# View\n<!-- hindsight:view:v1 -->\n\
             Webhook processing must be idempotent quokka.\n\
             All payments settle through Stripe zedonk.\n",
        )
        .unwrap();
    }
    std::fs::write(
        root.join(".agents/curation/migration/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.md"),
        "imported draft: Webhook processing must be idempotent quokka.\n",
    )
    .unwrap();
}

fn indexed_store(root: &std::path::Path) -> Store {
    let mut store = Store::open_in_memory().unwrap();
    index_repository_bounded(&mut store, root, None, None).unwrap();
    store
}

fn lexical_options() -> QueryOptions {
    QueryOptions {
        channels: QueryChannels::for_embedder(None),
        top_n: 25,
        diagnostics: true,
        ..QueryOptions::default()
    }
}

fn search(store: &Store, text: &str) -> snoop::runtime::QueryReport {
    query(store, None, text, &lexical_options()).unwrap()
}

fn evidences(report: &snoop::runtime::QueryReport) -> Vec<&str> {
    report
        .packet
        .items
        .iter()
        .map(|item| item.evidence_text.as_str())
        .collect()
}

fn contains(items: &[&str], needle: &str) -> bool {
    items.iter().any(|item| item.contains(needle))
}

fn memory_items(report: &snoop::runtime::QueryReport) -> Vec<&str> {
    report
        .packet
        .items
        .iter()
        .filter(|item| item.source_kind == SourceKind::HindsightMemory)
        .map(|item| item.evidence_text.as_str())
        .collect()
}

#[test]
fn ledger_indexed_as_special_source_and_derived_copies_suppressed() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let store = indexed_store(directory.path());

    let source = store
        .source_by_locator(".agents/curation/memory.json")
        .unwrap()
        .expect("canonical ledger is indexed");
    assert_eq!(source.kind, SourceKind::HindsightMemory);
    assert_eq!(
        store
            .units_for_source(".agents/curation/memory.json")
            .unwrap()
            .len(),
        8
    );

    let locators = store.source_locators().unwrap();
    assert!(
        locators
            .iter()
            .all(|locator| !locator.starts_with(".agents/engineering/")
                && !locator.contains("/migration/")
                && locator != ".agents/curation/memory.json"
                || *locator == ".agents/curation/memory.json"),
        "no generated view or migration copy is indexed: {locators:?}"
    );
    assert!(locators
        .iter()
        .all(|locator| !locator.contains("migration")));
}

#[test]
fn active_memory_retrieved_once_without_view_duplicates() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let report = search(
        &indexed_store(directory.path()),
        "how should the billing webhook handler process quokka retries",
    );
    let items = evidences(&report);
    assert!(
        contains(&items, "Webhook processing must be idempotent quokka"),
        "active authoritative memory is eligible: {items:?}"
    );
    assert!(
        contains(&items, "handleWebhook"),
        "curated memory complements current source: {items:?}"
    );
    assert_eq!(
        memory_items(&report)
            .iter()
            .filter(|item| item.contains("quokka"))
            .count(),
        1,
        "one unit per record despite two domain projections: {:?}",
        memory_items(&report)
    );
    assert!(
        report
            .packet
            .items
            .iter()
            .all(|item| !item.source_locator.starts_with(".agents/engineering/")),
        "no generated view copy joins the packet"
    );
}

#[test]
fn superseded_authoritative_record_hidden_by_default_despite_authority() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let report = search(
        &indexed_store(directory.path()),
        "who owns payment settlement zedonk",
    );
    let items = evidences(&report);
    assert!(
        contains(&items, "Enterprise invoices may settle manually"),
        "active replacement serves the ordinary query: {items:?}"
    );
    assert!(
        !contains(&items, "All payments settle through Stripe"),
        "superseded authoritative record stays invisible: {items:?}"
    );
    assert!(
        !contains(&items, "Stripe owns settlement exclusively"),
        "conflicted target stays invisible: {items:?}"
    );
    assert!(
        contains(&items, "ownership is disputed"),
        "active conflict record warns without settling: {items:?}"
    );
}

#[test]
fn evolution_query_unlocks_history_with_replacement_companion() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let store = indexed_store(directory.path());
    let report = search(
        &store,
        "did Stripe previously own all settlement history zedonk",
    );
    let items = evidences(&report);
    assert!(
        contains(&items, "All payments settle through Stripe"),
        "history unlocks the superseded record: {items:?}"
    );
    assert!(
        contains(&items, "Enterprise invoices may settle manually"),
        "the active replacement accompanies history: {items:?}"
    );
    let debug = report.debug.as_ref().unwrap();
    assert!(
        debug
            .items
            .iter()
            .any(|item| item.selected_because.iter().any(|reason| matches!(
                reason,
                snoop::core::SelectionReason::HindsightLifecycleExpansion(_)
            ))),
        "companion arrival is inspectable in diagnostics"
    );
}

#[test]
fn conflict_query_surfaces_disputed_targets() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let report = search(
        &indexed_store(directory.path()),
        "what is the settlement conflict zedonk",
    );
    let items = evidences(&report);
    assert!(contains(&items, "ownership is disputed"), "{items:?}");
    assert!(
        contains(&items, "Stripe owns settlement exclusively"),
        "conflict query unlocks disputed targets: {items:?}"
    );
    assert!(
        contains(&items, "do not treat as settled"),
        "disputed rendering warns explicitly: {items:?}"
    );
}

#[test]
fn unverified_hidden_by_default_and_unlocked_by_review_query() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let store = indexed_store(directory.path());
    let ordinary = search(&store, "cache layout mirrors production wombat");
    assert!(
        !contains(&evidences(&ordinary), "wombat"),
        "unverified stays out of ordinary retrieval"
    );
    let review = search(&store, "show pending unverified imported wombat knowledge");
    assert!(
        contains(
            &evidences(&review),
            "Legacy cache layout mirrors production"
        ),
        "review-state query unlocks unverified material"
    );
}

#[test]
fn resolved_conflict_is_history_only() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let store = indexed_store(directory.path());
    let ordinary = search(&store, "cache ownership dispute xylophone");
    assert!(
        !contains(&evidences(&ordinary), "xylophone"),
        "resolved conflict stays out of ordinary retrieval"
    );
    let history = search(&store, "history of cache ownership xylophone");
    assert!(
        contains(&evidences(&history), "dispute resolved"),
        "evolution query recovers resolved material"
    );
}

#[test]
fn scar_candidate_for_removal_stays_retrievable_and_annotated() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let report = search(
        &indexed_store(directory.path()),
        "mobile clients depend on v1 token semantics giraffe",
    );
    let items = evidences(&report);
    assert!(
        contains(&items, "Legacy authentication middleware remains"),
        "{items:?}"
    );
    assert!(
        contains(&items, "removal condition may need review"),
        "candidate-for-removal is annotated, not retired: {items:?}"
    );
}

#[test]
fn hindsight_diagnostics_carry_status_and_confidence() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let store = indexed_store(directory.path());
    let report = search(
        &store,
        "how should the billing webhook handler process quokka retries",
    );
    let debug = report.debug.as_ref().unwrap();
    let memory = debug
        .items
        .iter()
        .find(|item| {
            store
                .unit_by_id(item.unit_id.0)
                .unwrap()
                .is_some_and(|unit| unit.source_kind == SourceKind::HindsightMemory)
        })
        .expect("a memory item is diagnosed");
    assert!(
        memory.selected_because.iter().any(|reason| matches!(
            reason,
            snoop::core::SelectionReason::HindsightStatus(status) if status == "active"
        )),
        "status is inspectable: {:?}",
        memory.selected_because
    );
    assert!(
        memory.selected_because.iter().any(|reason| matches!(
            reason,
            snoop::core::SelectionReason::HindsightConfidence(confidence, multiplier)
                if confidence == "authoritative" && (*multiplier - 1.20).abs() < 1e-9
        )),
        "confidence prior is inspectable: {:?}",
        memory.selected_because
    );
}

#[test]
fn confidence_prior_prefers_authoritative_when_equally_relevant() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir_all(root.join(".agents/curation")).unwrap();
    let zebra = |n: u32, confidence: &str, statement: &str| {
        record(
            n,
            "constraint",
            "active",
            confidence,
            statement,
            serde_json::json!({}),
        )
    };
    std::fs::write(
        root.join(".agents/curation/memory.json"),
        ledger(&[
            zebra(11, "supported", "Zebra routing must be retried."),
            zebra(12, "authoritative", "Zebra routing must be idempotent."),
        ]),
    )
    .unwrap();
    let store = indexed_store(root);
    let report = search(&store, "zebra routing");
    let fused: Vec<i64> = report
        .debug
        .as_ref()
        .unwrap()
        .fused
        .iter()
        .map(|entry| entry.0)
        .collect();
    let id_of = |statement: &str| {
        store
            .units_for_source(".agents/curation/memory.json")
            .unwrap()
            .into_iter()
            .find(|unit| unit.evidence_text.contains(statement))
            .unwrap()
            .id
            .0
    };
    let authoritative = id_of("must be idempotent");
    let supported = id_of("must be retried");
    let rank = |id: i64| fused.iter().position(|candidate| *candidate == id).unwrap();
    assert!(
        rank(authoritative) < rank(supported),
        "authoritative modestly outranks equally relevant supported: {fused:?}"
    );
}

#[test]
fn hindsight_family_cap_bounds_curated_memory() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir_all(root.join(".agents/curation")).unwrap();
    let records: Vec<serde_json::Value> = (20..27)
        .map(|n| {
            record(
                n,
                "constraint",
                "active",
                "supported",
                &format!("Flibbertigibbet handling rule number {n} stands."),
                serde_json::json!({}),
            )
        })
        .collect();
    std::fs::write(root.join(".agents/curation/memory.json"), ledger(&records)).unwrap();
    let report = search(&indexed_store(root), "flibbertigibbet handling rule");
    assert_eq!(
        memory_items(&report).len(),
        5,
        "family cap admits five related memories, not seven"
    );
}

#[test]
fn irrelevant_authoritative_memory_does_not_defeat_relevant_source() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let report = search(
        &indexed_store(directory.path()),
        "handleWebhook processedEvents set",
    );
    let items = evidences(&report);
    assert!(
        contains(&items, "handleWebhook"),
        "strongly relevant source still serves: {items:?}"
    );
}

#[test]
fn invalid_ledger_indexes_neither_memories_nor_projections() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir_all(root.join(".agents/curation")).unwrap();
    std::fs::create_dir_all(root.join(".agents/engineering")).unwrap();
    std::fs::write(
        root.join(".agents/curation/memory.json"),
        r#"{"version": 99, "records": []}"#,
    )
    .unwrap();
    std::fs::write(
        root.join(".agents/engineering/ARCHITECTURE.md"),
        "# View\n<!-- hindsight:view:v1 -->\nQuokka fallback claims.\n",
    )
    .unwrap();
    let store = indexed_store(root);
    assert!(
        store
            .source_by_locator(".agents/curation/memory.json")
            .unwrap()
            .is_none(),
        "unusable ledger commits no source"
    );
    let report = search(&store, "quokka fallback claims");
    assert!(
        report.packet.items.is_empty(),
        "no silent fallback to generated views: {:?}",
        evidences(&report)
    );
}
