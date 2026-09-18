//! Hindsight canonical-ledger adapter.
//!
//! Snoop ingests `.agents/curation/memory.json` — Hindsight's canonical
//! state — and deliberately does not ingest the deterministic generated
//! views (`.agents/engineering/*.md`), migration archives, session copies,
//! or run reports derived from it. One Snoop retrieval unit is built per
//! memory record, even when the record projects into several domains.
//!
//! Single owner of the ledger locator: the scanner force-scans this path
//! and routes it here, so content sniffing never decides routing (the same
//! pattern as the cheatcodes corpus).

use std::path::{Path, PathBuf};

use crate::core::{hash_segments, AnchorKind, BuiltAnchor, BuiltUnit, UnitKind};
use crate::ingest::units::estimate_tokens;
use crate::metadata::hindsight::{self, HindsightMeta};

/// Canonical Hindsight ledger. A repository is Hindsight-managed only when
/// this regular file exists; generated views alone never count.
pub const HINDSIGHT_LEDGER: &str = ".agents/curation/memory.json";

/// Marker emitted at the top of every generated Hindsight projection.
/// Without a ledger, files carrying this marker are suppressed from normal
/// ingestion instead of being treated as authored documentation.
pub const GENERATED_VIEW_MARKER: &str = "<!-- hindsight:view:v1 -->";

/// The only ledger schema version this adapter understands. Anything else
/// fails closed (see [`LedgerError`]).
pub const SUPPORTED_LEDGER_VERSION: i64 = 1;

pub struct HindsightInstallation {
    pub ledger: PathBuf,
}

/// A repository is Hindsight-managed when the canonical ledger file exists.
/// Generated `.agents/engineering/` views alone never qualify.
pub fn detect(root: &Path) -> Option<HindsightInstallation> {
    let ledger = root.join(HINDSIGHT_LEDGER);
    ledger.is_file().then_some(HindsightInstallation { ledger })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    Parse(String),
    UnsupportedVersion(String),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(detail) => write!(f, "invalid Hindsight ledger: {detail}"),
            Self::UnsupportedVersion(detail) => {
                write!(f, "unsupported Hindsight ledger version: {detail}")
            }
        }
    }
}

impl std::error::Error for LedgerError {}

#[derive(Debug, serde::Deserialize)]
struct Ledger {
    #[serde(default)]
    version: serde_json::Value,
    #[serde(default)]
    records: Vec<Record>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct Scope {
    #[serde(default)]
    global: bool,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    symbols: Vec<String>,
    #[serde(default)]
    concepts: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct Provenance {
    #[serde(default, rename = "type")]
    provenance_type: String,
    #[serde(default, rename = "ref")]
    reference: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct Conflict {
    #[serde(default)]
    targets: Vec<String>,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct Record {
    #[serde(default)]
    id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    confidence: String,
    #[serde(default)]
    basis: String,
    #[serde(default)]
    domains: Vec<String>,
    #[serde(default)]
    statement: String,
    #[serde(default)]
    scope: Scope,
    #[serde(default)]
    revision: i64,
    #[serde(default)]
    supersedes: Vec<String>,
    #[serde(default)]
    contradicts: Vec<String>,
    #[serde(default, rename = "createdAt")]
    created_at: Option<String>,
    #[serde(default, rename = "lastValidatedAt")]
    last_validated_at: Option<String>,
    #[serde(default)]
    provenance: Vec<Provenance>,
    #[serde(default)]
    constraint: Option<String>,
    #[serde(default, rename = "removalCondition")]
    removal_condition: Option<String>,
    #[serde(default, rename = "scarState")]
    scar_state: Option<String>,
    #[serde(default)]
    conflict: Option<Conflict>,
}

/// Confidence prior applied after lifecycle eligibility, inside the
/// eligible class only. `conflicted` confidence is handled by the
/// lifecycle policy and carries no multiplier.
pub fn confidence_multiplier(confidence: &str) -> f64 {
    match confidence {
        "authoritative" => 1.20,
        "strong" => 1.12,
        "supported" => 1.05,
        "inferred" => 0.92,
        _ => 1.0,
    }
}

/// Parse and validate the canonical ledger. Fails closed on malformed
/// JSON or an unsupported schema version; individual records missing an
/// identity or statement are skipped loudly, never promoted.
pub(crate) fn parse_ledger(content: &str) -> Result<Vec<Record>, LedgerError> {
    let ledger: Ledger =
        serde_json::from_str(content).map_err(|error| LedgerError::Parse(error.to_string()))?;
    let version = ledger.version.as_i64();
    if version != Some(SUPPORTED_LEDGER_VERSION) {
        return Err(LedgerError::UnsupportedVersion(format!(
            "expected version {}, found {}",
            SUPPORTED_LEDGER_VERSION, ledger.version
        )));
    }
    Ok(ledger.records)
}

fn is_exact_path(path: &str) -> bool {
    !path.is_empty() && !path.contains(['*', '?', '[', '{', '!', '\\'])
}

fn strip_prefix<'a>(reference: &'a str, prefixes: &[&str]) -> &'a str {
    for prefix in prefixes {
        if let Some(rest) = reference.strip_prefix(prefix) {
            return rest;
        }
    }
    reference
}

fn provenance_anchor(provenance: &Provenance) -> Option<BuiltAnchor> {
    let supported = || "supported_by".to_string();
    match provenance.provenance_type.as_str() {
        "source" => {
            let path = strip_prefix(&provenance.reference, &["file:", "index:", "head:"]);
            (!path.is_empty() && path != provenance.reference).then(|| BuiltAnchor {
                kind: AnchorKind::File,
                value: path.to_string(),
                relationship: supported(),
            })
        }
        "decision" | "doc" => {
            let path = strip_prefix(&provenance.reference, &["file:"]);
            (!path.is_empty() && (path.contains('/') || path.contains('.'))).then(|| BuiltAnchor {
                kind: AnchorKind::File,
                value: path.to_string(),
                relationship: supported(),
            })
        }
        "session" => {
            let id = strip_prefix(&provenance.reference, &["session:"]);
            (!id.is_empty()).then(|| BuiltAnchor {
                kind: AnchorKind::Session,
                value: id.to_string(),
                relationship: supported(),
            })
        }
        "history" => {
            let oid = strip_prefix(&provenance.reference, &["git:"]);
            (!oid.is_empty()).then(|| BuiltAnchor {
                kind: AnchorKind::Commit,
                value: oid.to_string(),
                relationship: supported(),
            })
        }
        _ => None,
    }
}

fn scope_summary(record: &Record) -> String {
    if record.scope.global {
        return "Repository-wide".to_string();
    }
    let mut selectors = Vec::new();
    selectors.extend(record.scope.paths.iter().cloned());
    selectors.extend(record.scope.symbols.iter().cloned());
    selectors.extend(record.scope.concepts.iter().cloned());
    if selectors.is_empty() {
        "Repository-wide".to_string()
    } else {
        selectors.join(", ")
    }
}

fn validated_date(record: &Record) -> Option<&str> {
    record
        .last_validated_at
        .as_deref()
        .and_then(|stamp| stamp.split('T').next())
}

/// Concise agent-facing evidence text. The lifecycle label stays visible
/// here because the normal context item exposes evidence text but not
/// arbitrary unit metadata.
fn evidence_text(record: &Record) -> String {
    let mut text = String::new();
    if record.kind == "conflict" && record.status == "active" {
        text.push_str(&format!(
            "[Hindsight: unresolved conflict; {}]\n\n",
            record.id
        ));
    } else if record.status == "conflicted" {
        text.push_str("[Hindsight disputed memory — do not treat as settled]\n\n");
        text.push_str(&format!(
            "[Hindsight: conflicted {} {}; {}]\n\n",
            record.confidence, record.kind, record.id
        ));
    } else if record.status == "unverified" {
        text.push_str("[Hindsight unverified memory — review before relying on it]\n\n");
        text.push_str(&format!(
            "[Hindsight: unverified {} {}; {}]\n\n",
            record.confidence, record.kind, record.id
        ));
    } else if matches!(
        record.status.as_str(),
        "superseded" | "obsolete" | "resolved"
    ) {
        text.push_str(&format!(
            "[Hindsight historical memory — {}; do not treat as current]\n\n",
            record.status
        ));
        text.push_str(&format!(
            "[Hindsight: {} {} {}; {}]\n\n",
            record.status, record.confidence, record.kind, record.id
        ));
    } else if record.kind == "scar" {
        text.push_str(&format!(
            "[Hindsight: active {} scar; {}]\n\n",
            record.confidence, record.id
        ));
    } else {
        text.push_str(&format!(
            "[Hindsight: {} {} {}; {}]\n\n",
            record.status, record.confidence, record.kind, record.id
        ));
    }
    text.push_str(&record.statement);
    if record.kind == "scar" {
        if let Some(constraint) = record.constraint.as_deref().filter(|s| !s.is_empty()) {
            text.push_str(&format!("\n\nConstraint: {constraint}"));
        }
        if let Some(condition) = record
            .removal_condition
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            text.push_str(&format!("\nRemoval condition: {condition}"));
        }
        if record.scar_state.as_deref() == Some("candidate_for_removal") {
            text.push_str("\n[Hindsight active scar — removal condition may need review]");
        }
    }
    if record.kind == "conflict" {
        if let Some(conflict) = record.conflict.as_ref() {
            if !conflict.reason.is_empty() {
                text.push_str(&format!("\n\nReason: {}", conflict.reason));
            }
        }
    }
    text.push_str(&format!("\n\nScope: {}", scope_summary(record)));
    if let Some(date) = validated_date(record) {
        text.push_str(&format!("\nLast validated: {date}"));
    }
    text
}

/// Full searchable classification. Terms that should not clutter the
/// human-facing evidence text live here instead.
fn routing_text(record: &Record) -> String {
    let mut lines = vec![
        "source: hindsight_memory".to_string(),
        format!("memory_id: {}", record.id),
        format!("kind: {}", record.kind),
        format!("status: {}", record.status),
        format!("confidence: {}", record.confidence),
        format!("basis: {}", record.basis),
        format!("domains: {}", record.domains.join(" ")),
    ];
    for path in &record.scope.paths {
        lines.push(format!("scope_path: {path}"));
    }
    for symbol in &record.scope.symbols {
        lines.push(format!("scope_symbol: {symbol}"));
    }
    for concept in &record.scope.concepts {
        lines.push(format!("scope_concept: {concept}"));
    }
    lines.join("\n")
}

fn anchors(record: &Record) -> Vec<BuiltAnchor> {
    let mut anchors = vec![BuiltAnchor {
        kind: AnchorKind::Memory,
        value: record.id.clone(),
        relationship: "self".to_string(),
    }];
    for target in &record.supersedes {
        anchors.push(BuiltAnchor {
            kind: AnchorKind::Memory,
            value: target.clone(),
            relationship: "supersedes".to_string(),
        });
    }
    for target in &record.contradicts {
        anchors.push(BuiltAnchor {
            kind: AnchorKind::Memory,
            value: target.clone(),
            relationship: "contradicts".to_string(),
        });
    }
    if let Some(conflict) = record.conflict.as_ref() {
        for target in &conflict.targets {
            anchors.push(BuiltAnchor {
                kind: AnchorKind::Memory,
                value: target.clone(),
                relationship: "conflict_target".to_string(),
            });
        }
    }
    // Only exact paths become file anchors. Broad globs stay in
    // metadata/routing text instead of exploding into file links.
    for path in &record.scope.paths {
        if is_exact_path(path) {
            anchors.push(BuiltAnchor {
                kind: AnchorKind::File,
                value: path.clone(),
                relationship: "applies_to".to_string(),
            });
        }
    }
    for symbol in &record.scope.symbols {
        anchors.push(BuiltAnchor {
            kind: AnchorKind::Symbol,
            value: symbol.clone(),
            relationship: "applies_to".to_string(),
        });
    }
    anchors.extend(record.provenance.iter().filter_map(provenance_anchor));
    anchors
}

fn unit_hash(record: &Record) -> String {
    let scope = serde_json::json!({
        "global": record.scope.global,
        "paths": record.scope.paths,
        "symbols": record.scope.symbols,
        "concepts": record.scope.concepts,
    })
    .to_string();
    let conflict_reason = record
        .conflict
        .as_ref()
        .map(|conflict| conflict.reason.clone())
        .unwrap_or_default();
    let conflict_targets = record
        .conflict
        .as_ref()
        .map(|conflict| conflict.targets.join(","))
        .unwrap_or_default();
    hash_segments(&[
        hindsight::POLICY_VERSION,
        &record.id,
        &record.revision.to_string(),
        &record.statement,
        &scope,
        &record.kind,
        &record.status,
        &record.confidence,
        &record.basis,
        record.scar_state.as_deref().unwrap_or(""),
        record.constraint.as_deref().unwrap_or(""),
        record.removal_condition.as_deref().unwrap_or(""),
        &conflict_reason,
        &conflict_targets,
        &record.supersedes.join(","),
        &record.contradicts.join(","),
    ])
}

fn build_unit(record: &Record, schema_version: i64) -> BuiltUnit {
    let evidence = evidence_text(record);
    let routing = routing_text(record);
    let mut metadata = serde_json::json!({});
    hindsight::set(
        &mut metadata,
        &HindsightMeta {
            schema_version,
            record_id: record.id.clone(),
            revision: record.revision,
            kind: record.kind.clone(),
            status: record.status.clone(),
            confidence: record.confidence.clone(),
            basis: record.basis.clone(),
            domains: record.domains.clone(),
            scope_paths: record.scope.paths.clone(),
            scope_symbols: record.scope.symbols.clone(),
            scope_concepts: record.scope.concepts.clone(),
            scope_global: record.scope.global,
            supersedes: record.supersedes.clone(),
            contradicts: record.contradicts.clone(),
            created_at: record.created_at.clone(),
            last_validated_at: record.last_validated_at.clone(),
            scar_state: record.scar_state.clone(),
            scar_constraint: record.constraint.clone(),
            scar_removal_condition: record.removal_condition.clone(),
            conflict_targets: record
                .conflict
                .as_ref()
                .map(|conflict| conflict.targets.clone())
                .unwrap_or_default(),
            conflict_reason: record
                .conflict
                .as_ref()
                .map(|conflict| conflict.reason.clone()),
            provenance_refs: record
                .provenance
                .iter()
                .map(|provenance| provenance.reference.clone())
                .collect(),
        },
    );
    BuiltUnit {
        kind: UnitKind::Memory,
        token_count: estimate_tokens(&evidence),
        content_hash: unit_hash(record),
        evidence_text: evidence,
        routing_text: routing,
        metadata,
        anchors: anchors(record),
    }
}

/// Build one retrieval unit per memory record. A record exposed to several
/// domains still yields exactly one unit keyed by its memory ID.
pub fn ingest_ledger(content: &str) -> Result<Vec<BuiltUnit>, LedgerError> {
    let records = parse_ledger(content)?;
    let mut units = Vec::with_capacity(records.len());
    for record in &records {
        if record.id.is_empty() || record.statement.trim().is_empty() {
            eprintln!("warning: skipped Hindsight record without an identity or statement");
            continue;
        }
        units.push(build_unit(record, SUPPORTED_LEDGER_VERSION));
    }
    Ok(units)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn record_id(n: u8) -> String {
        format!("mem_{:032x}", n)
    }

    pub(crate) fn ledger_json(records: &[serde_json::Value]) -> String {
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

    pub(crate) fn basic_record(id: &str, status: &str, confidence: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "revision": 1,
            "kind": "constraint",
            "domains": ["architecture"],
            "statement": "Webhook processing must be idempotent.",
            "scope": {"global": false, "paths": ["src/billing/**"], "symbols": [], "concepts": ["billing"]},
            "exposeTo": ["ARCHITECTURE.md"],
            "status": status,
            "confidence": confidence,
            "basis": "policy",
            "createdAt": "2026-09-18T00:00:00Z",
            "lastValidatedAt": "2026-09-18T00:00:00Z",
            "provenance": [{"type": "source", "ref": "file:src/billing/webhook.ts", "hash": "h", "classification": "observed", "reportPath": "r", "at": "t", "start": 0, "end": 1}],
            "supersedes": [],
            "contradicts": [],
        })
    }

    #[test]
    fn rejects_malformed_json_and_unsupported_versions() {
        assert!(matches!(
            parse_ledger("not json"),
            Err(LedgerError::Parse(_))
        ));
        assert!(matches!(
            parse_ledger(r#"{"version": 2, "records": []}"#),
            Err(LedgerError::UnsupportedVersion(_))
        ));
        assert!(matches!(
            parse_ledger(r#"{"records": []}"#),
            Err(LedgerError::UnsupportedVersion(_))
        ));
        assert!(parse_ledger(&ledger_json(&[])).unwrap().is_empty());
    }

    #[test]
    fn one_unit_per_record_despite_multiple_domains() {
        let mut record = basic_record(&record_id(1), "active", "authoritative");
        record["domains"] = serde_json::json!(["architecture", "operations"]);
        record["exposeTo"] = serde_json::json!(["ARCHITECTURE.md", "OPERATIONS.md"]);
        let units = ingest_ledger(&ledger_json(&[record])).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].kind, UnitKind::Memory);
    }

    #[test]
    fn lifecycle_label_stays_visible_in_evidence_text() {
        let units = ingest_ledger(&ledger_json(&[basic_record(
            &record_id(2),
            "active",
            "authoritative",
        )]))
        .unwrap();
        assert!(units[0]
            .evidence_text
            .contains("active authoritative constraint"));
        assert!(units[0].evidence_text.contains(&record_id(2)));
        assert!(units[0].evidence_text.contains("src/billing/**"));

        let units = ingest_ledger(&ledger_json(&[basic_record(
            &record_id(3),
            "superseded",
            "authoritative",
        )]))
        .unwrap();
        assert!(units[0].evidence_text.contains("superseded"));
        assert!(units[0].evidence_text.contains("do not treat as current"));
    }

    #[test]
    fn status_change_rehashes_the_unit() {
        let active = ingest_ledger(&ledger_json(&[basic_record(
            &record_id(4),
            "active",
            "authoritative",
        )]))
        .unwrap();
        let retired = ingest_ledger(&ledger_json(&[basic_record(
            &record_id(4),
            "superseded",
            "authoritative",
        )]))
        .unwrap();
        assert_ne!(active[0].content_hash, retired[0].content_hash);
    }

    #[test]
    fn scar_and_conflict_render_their_lifecycle() {
        let mut scar = basic_record(&record_id(5), "active", "strong");
        scar["kind"] = serde_json::json!("scar");
        scar["reason"] = serde_json::json!("legacy clients");
        scar["constraint"] = serde_json::json!("Do not add new auth behavior here.");
        scar["removalCondition"] = serde_json::json!("Remove after v1 sunset.");
        scar["scarState"] = serde_json::json!("candidate_for_removal");
        scar["removalSignals"] = serde_json::json!([]);
        let units = ingest_ledger(&ledger_json(&[scar])).unwrap();
        assert!(units[0].evidence_text.contains("Constraint:"));
        assert!(units[0]
            .evidence_text
            .contains("removal condition may need review"));

        let mut conflict = basic_record(&record_id(6), "active", "conflicted");
        conflict["kind"] = serde_json::json!("conflict");
        conflict["conflict"] = serde_json::json!({"targets": [record_id(5)], "reason": "Manual settlement conflicts."});
        let units = ingest_ledger(&ledger_json(&[conflict])).unwrap();
        assert!(units[0].evidence_text.contains("unresolved conflict"));
        assert!(units[0].anchors.iter().any(|anchor| {
            anchor.kind == AnchorKind::Memory
                && anchor.value == record_id(5)
                && anchor.relationship == "conflict_target"
        }));
    }

    #[test]
    fn anchors_cover_scope_provenance_and_supersession() {
        let mut record = basic_record(&record_id(7), "active", "strong");
        record["scope"] = serde_json::json!({"global": false, "paths": ["src/exact/file.ts", "src/**"], "symbols": ["SubscriptionService"], "concepts": []});
        record["supersedes"] = serde_json::json!([record_id(8)]);
        record["provenance"] = serde_json::json!([
            {"type": "session", "ref": "session:abc", "hash": "h", "classification": "observed", "reportPath": "r", "at": "t", "start": 0, "end": 1},
            {"type": "history", "ref": "git:deadbee", "hash": "h", "classification": "observed", "reportPath": "r", "at": "t", "start": 0, "end": 1},
        ]);
        let units = ingest_ledger(&ledger_json(&[record])).unwrap();
        let anchors = &units[0].anchors;
        assert!(anchors
            .iter()
            .any(|anchor| anchor.kind == AnchorKind::Memory && anchor.relationship == "self"));
        assert!(anchors
            .iter()
            .any(|anchor| anchor.kind == AnchorKind::Memory
                && anchor.value == record_id(8)
                && anchor.relationship == "supersedes"));
        assert!(anchors.iter().any(|anchor| anchor.kind == AnchorKind::File
            && anchor.value == "src/exact/file.ts"
            && anchor.relationship == "applies_to"));
        assert!(
            !anchors
                .iter()
                .any(|anchor| anchor.kind == AnchorKind::File && anchor.value == "src/**"),
            "broad globs stay in metadata, not anchors"
        );
        assert!(anchors.iter().any(
            |anchor| anchor.kind == AnchorKind::Symbol && anchor.value == "SubscriptionService"
        ));
        assert!(anchors
            .iter()
            .any(|anchor| anchor.kind == AnchorKind::Session && anchor.value == "abc"));
        assert!(anchors
            .iter()
            .any(|anchor| anchor.kind == AnchorKind::Commit && anchor.value == "deadbee"));
    }
}
