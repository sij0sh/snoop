//! Muse session ingestion: one episode unit per completed root run.
//!
//! Muse keeps event-sourced transcripts under `~/.local/share/muse`
//! (overridable with `SNOOP_MUSE_ROOT`) plus a `session-index.db` SQLite
//! index attributing each session to a workspace root. This adapter maps
//! the sealed prefix of each ended session — everything up to the final
//! durable `session.end` record — to episode units under
//! `muse-session:<root-session-id>` locators.
//!
//! The searchable projection is deliberately lossy: raw tool-result bodies,
//! streamed task output, and encrypted reasoning stay in the Muse logs and
//! never enter the index. The reader itself preserves envelope provenance
//! (stream id, record id, sequence, physical line ordinal, frame position)
//! so schema drift surfaces as a loud per-session skip instead of silent
//! corruption.
//!
//! Child (subagent) transcripts are deferred: this slice indexes root runs
//! only. Unknown payloads are preserved by the reader and skipped by the
//! evidence projection.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::core::{hash_segments, AnchorKind, BuiltAnchor, BuiltUnit, UnitKind};
use crate::ingest::units::{estimate_tokens, split_episode_pieces, MAX_TOKENS};

use super::tools::{tool_ref, upgrade_outcome_from_text, ToolOutcome, ToolRef};
use super::MAX_EPISODES_PER_SESSION;

/// Bumped whenever the run-to-unit policy changes so stored hashes change.
pub const MUSE_POLICY_VERSION: &str = "muse-run-v1";

/// Transcript layout this adapter reads.
const SUPPORTED_LAYOUT: &str = "session_jsonl";
/// Session status this adapter reads. Ended sessions are selected later by
/// their durable end marker, never by this column.
const SUPPORTED_STATUS: &str = "valid";

/// Locator namespace for Muse root sessions. The `pi-session:` namespace
/// is unchanged.
pub fn muse_session_locator(session_id: &str) -> String {
    format!("muse-session:{session_id}")
}

/// Muse data root: explicit override first, then the default location
/// under the caller's home directory.
pub fn muse_root() -> Option<PathBuf> {
    if let Some(override_root) = std::env::var_os("SNOOP_MUSE_ROOT") {
        return Some(PathBuf::from(override_root));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/muse"))
}

/// One indexed Muse session: the transcript path plus the optional
/// attribution fields carried by the index row.
#[derive(Debug, Clone)]
pub struct MuseSession {
    pub session_id: String,
    pub log_path: PathBuf,
    pub session_name: Option<String>,
    pub workspace_root: Option<String>,
    pub model_id: Option<String>,
    pub created_at_us: Option<i64>,
    pub updated_at_us: Option<i64>,
    pub source_fingerprint: Option<String>,
}

/// Discovers this repository's Muse sessions through the read-only index.
/// A missing index file is a successful empty discovery (Muse never ran
/// here). Any other index failure is an error so the caller retains
/// previously indexed `muse-session:` sources instead of purging them.
///
/// Logs with no index row are deferred: without a canonical workspace
/// association they cannot be attributed safely.
pub fn discover_muse_sessions(
    repo_root: &Path,
) -> Result<Vec<MuseSession>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(root) = muse_root() else {
        return Ok(Vec::new());
    };
    let index_path = root.join("session-index.db");
    if !index_path.exists() {
        return Ok(Vec::new());
    }
    let connection = rusqlite::Connection::open_with_flags(
        &index_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|error| {
        format!(
            "muse session index unreadable ({}): {error}",
            index_path.display()
        )
    })?;
    let canonical_repo = repo_root.canonicalize().ok();
    let mut statement = connection
        .prepare(
            "SELECT session_id, session_log_path, layout, workspace_root, status, \
             session_name, model_id, created_at_us, updated_at_us, source_fingerprint \
             FROM sessions",
        )
        .map_err(|error| format!("muse session index query failed: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })
        .map_err(|error| format!("muse session index query failed: {error}"))?;
    let mut sessions = Vec::new();
    for row in rows {
        let (
            session_id,
            log_path,
            layout,
            workspace_root,
            status,
            session_name,
            model_id,
            created_at_us,
            updated_at_us,
            source_fingerprint,
        ) = row.map_err(|error| format!("muse session index row unreadable: {error}"))?;
        if layout != SUPPORTED_LAYOUT {
            eprintln!(
                "warning: skipped Muse session {session_id} with unsupported layout {layout:?}"
            );
            continue;
        }
        if status != SUPPORTED_STATUS {
            eprintln!(
                "warning: skipped Muse session {session_id} with unsupported status {status:?}"
            );
            continue;
        }
        // The index attribution key: only the canonical workspace root
        // decides. Unresolvable or foreign roots are never cross-indexed.
        let belongs = match (&workspace_root, &canonical_repo) {
            (Some(workspace), Some(repo)) => std::fs::canonicalize(workspace)
                .ok()
                .is_some_and(|canonical| canonical == *repo),
            _ => false,
        };
        if !belongs {
            continue;
        }
        let mut log_path = PathBuf::from(log_path);
        if log_path.is_relative() {
            log_path = root.join(log_path);
        }
        if !path_within_root(&log_path, &root) {
            eprintln!(
                "warning: skipped Muse session {session_id} whose log path escapes the Muse root"
            );
            continue;
        }
        sessions.push(MuseSession {
            session_id,
            log_path,
            session_name,
            workspace_root,
            model_id,
            created_at_us,
            updated_at_us,
            source_fingerprint,
        });
    }
    sessions.sort_by(|a, b| a.log_path.cmp(&b.log_path));
    Ok(sessions)
}

/// Prefers canonical containment; falls back to the lexical prefix so
/// not-yet-written logs stay attributable.
fn path_within_root(path: &Path, root: &Path) -> bool {
    if let (Ok(canonical_path), Ok(canonical_root)) = (path.canonicalize(), root.canonicalize()) {
        return canonical_path.starts_with(&canonical_root);
    }
    path.starts_with(root)
}

/// One decoded transcript record with enough provenance to debug schema
/// drift. `log_ordinal` is the 1-based physical line number;
/// `frame_child_index` is set only for records decoded out of a
/// transaction frame's double-encoded children.
#[derive(Debug, Clone)]
pub struct MuseRecord {
    pub stream_session_id: String,
    pub record_id: String,
    pub sequence: u64,
    pub recorded_at_us: i64,
    pub record_type: String,
    pub durability: String,
    pub causation_id: Option<String>,
    pub envelope_schema_version: u32,
    pub payload_type: String,
    pub payload_schema_version: u32,
    pub log_ordinal: usize,
    pub frame_child_index: Option<usize>,
    pub start_byte: usize,
    pub end_byte: usize,
    pub payload: serde_json::Value,
}

/// A decoded transcript plus its sealed prefix: the byte length sheltering
/// under the final durable end marker, if any.
#[derive(Debug, Clone)]
pub struct ParsedLog {
    pub records: Vec<MuseRecord>,
    /// Byte length of `content[..]` covered by the final durable end
    /// marker. `None` means no durable end exists and the session
    /// contributes no units.
    pub sealed_len: Option<usize>,
    pub sealed_end_sequence: Option<u64>,
}

/// Rejects the whole session when the envelope schema is unknown.
#[derive(Debug)]
pub enum MuseReadError {
    UnsupportedEnvelope(u32),
}

impl std::fmt::Display for MuseReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedEnvelope(version) => {
                write!(f, "unsupported Muse envelope schema version {version}")
            }
        }
    }
}

impl std::error::Error for MuseReadError {}

fn record_from_value(
    value: &serde_json::Value,
    log_ordinal: usize,
    frame_child_index: Option<usize>,
    start_byte: usize,
    end_byte: usize,
) -> Result<Option<MuseRecord>, MuseReadError> {
    let Some(envelope_schema_version) = value.get("schema_version").and_then(|v| v.as_u64()) else {
        eprintln!("warning: skipped Muse line {log_ordinal} without an envelope schema version");
        return Ok(None);
    };
    if envelope_schema_version != 1 {
        return Err(MuseReadError::UnsupportedEnvelope(
            envelope_schema_version as u32,
        ));
    }
    let string_field = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    Ok(Some(MuseRecord {
        stream_session_id: value
            .get("stream")
            .and_then(|s| s.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        record_id: string_field("id"),
        sequence: value
            .get("sequence")
            .and_then(|v| v.as_u64())
            .unwrap_or(u64::MAX),
        recorded_at_us: value
            .get("recorded_at")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        record_type: string_field("record_type"),
        durability: string_field("durability"),
        causation_id: value
            .get("causation_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        envelope_schema_version: envelope_schema_version as u32,
        payload_type: string_field("payload_type"),
        payload_schema_version: value
            .get("payload_schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        log_ordinal,
        frame_child_index,
        start_byte,
        end_byte,
        payload: value
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    }))
}

/// Streams the transcript, decoding plain records and every frame child.
/// Omitted live-only markers leave sequence gaps, never evidence. A torn
/// final line from an actively appended file is ignored; malformed lines
/// elsewhere are skipped loudly. Records order by child sequence within
/// each stream, not by physical line order.
pub fn read_muse_log(content: &str) -> Result<ParsedLog, MuseReadError> {
    let mut records = Vec::new();
    let mut byte_offset = 0usize;
    let mut ordinal = 0usize;
    // Split first so the torn-tail check sees physical position.
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let last_content = lines.iter().rposition(|line| !line.trim().is_empty());
    for (index, line) in lines.iter().enumerate() {
        let start_byte = byte_offset;
        byte_offset += line.len();
        let end_byte = byte_offset;
        if line.trim().is_empty() {
            continue;
        }
        ordinal += 1;
        let trimmed = line.trim();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            if Some(index) == last_content {
                // Torn write at the live tail: not an error.
                break;
            }
            eprintln!("warning: skipped malformed Muse line {ordinal}");
            continue;
        };
        if let Some(frame) = value.get("retained_frame") {
            if frame.as_str() != Some("session_permission_transaction") {
                eprintln!("warning: skipped Muse line {ordinal} with unknown frame {frame}");
                continue;
            }
            let children = value
                .get("children")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for child in &children {
                let child_index = child.get("child_index").and_then(|v| v.as_u64());
                let Some(encoded) = child.get("record_json").and_then(|v| v.as_str()) else {
                    eprintln!("warning: skipped Muse frame child without record_json");
                    continue;
                };
                let Ok(inner) = serde_json::from_str::<serde_json::Value>(encoded) else {
                    eprintln!("warning: skipped undecodable Muse frame child");
                    continue;
                };
                if let Some(record) = record_from_value(
                    &inner,
                    ordinal,
                    child_index.map(|v| v as usize),
                    start_byte,
                    end_byte,
                )? {
                    records.push(record);
                }
            }
            continue;
        }
        if value.get("retained_marker").is_some() {
            // Omitted live-only sequence: provenance gap, not evidence.
            continue;
        }
        if let Some(record) = record_from_value(&value, ordinal, None, start_byte, end_byte)? {
            records.push(record);
        }
    }
    // Child sequence is the stream order; the outer line ordinal is only
    // physical provenance (a frame at ordinal 1 holds sequences 1 and 2).
    records.sort_by_key(|record| record.sequence);
    let mut sealed_len = None;
    let mut sealed_end_sequence = None;
    for record in &records {
        if record.payload_type == "session.end" && record.durability == "durable" {
            sealed_len = Some(record.end_byte);
            sealed_end_sequence = Some(record.sequence);
        }
    }
    Ok(ParsedLog {
        records,
        sealed_len,
        sealed_end_sequence,
    })
}

fn sealed_records(log: &ParsedLog) -> &[MuseRecord] {
    let Some(sealed_len) = log.sealed_len else {
        return &[];
    };
    let end = log
        .records
        .iter()
        .rposition(|record| {
            record.payload_type == "session.end"
                && record.durability == "durable"
                && record.end_byte <= sealed_len
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    &log.records[..end]
}

/// One committed tool call. Items are pushed in record sequence order,
/// so the evidence order needs no extra sequence field.
#[derive(Debug, Clone)]
struct MuseToolCall {
    call_id: String,
    name: String,
    reference: ToolRef,
}

/// Ordered evidence item inside one run.
#[derive(Debug, Clone)]
enum RunItem {
    AssistantText { text: String },
    ToolCall { call: MuseToolCall },
    Reasoning { text: String },
}

/// Accumulates one root `run_id` in sequence order. Usage sums across
/// every `model_completed` step; per-step usage is never overwritten.
struct RunAccumulator {
    run_id: String,
    start_sequence: u64,
    start_record_id: String,
    end_sequence: u64,
    end_record_id: String,
    timestamp_secs: Option<i64>,
    prompt: Option<String>,
    items: Vec<RunItem>,
    /// `tool_call_id` to committed result body. Bodies never enter
    /// evidence; they only upgrade structured bash outcomes.
    results: HashMap<String, String>,
    usage: BTreeMap<String, i64>,
    model: Option<String>,
    terminal: bool,
}

impl RunAccumulator {
    fn new(run_id: &str, record: &MuseRecord) -> Self {
        Self {
            run_id: run_id.to_string(),
            start_sequence: record.sequence,
            start_record_id: record.record_id.clone(),
            end_sequence: record.sequence,
            end_record_id: record.record_id.clone(),
            timestamp_secs: None,
            prompt: None,
            items: Vec::new(),
            results: HashMap::new(),
            usage: BTreeMap::new(),
            model: None,
            terminal: false,
        }
    }

    fn touch(&mut self, record: &MuseRecord) {
        if record.sequence < self.start_sequence {
            self.start_sequence = record.sequence;
            self.start_record_id.clone_from(&record.record_id);
            if self.timestamp_secs.is_none() {
                self.timestamp_secs = micros_to_secs(record.recorded_at_us);
            }
        }
        if record.sequence >= self.end_sequence {
            self.end_sequence = record.sequence;
            self.end_record_id.clone_from(&record.record_id);
        }
        if self.timestamp_secs.is_none() {
            self.timestamp_secs = micros_to_secs(record.recorded_at_us);
        }
    }
}

fn micros_to_secs(recorded_at_us: i64) -> Option<i64> {
    (recorded_at_us > 0).then(|| recorded_at_us.div_euclid(1_000_000))
}

/// Parses the double-encoded tool-call arguments, keeping the raw text as
/// the summary fallback when it is not valid JSON.
fn parse_call_arguments(raw: &str) -> (serde_json::Value, Option<String>) {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) if value.is_object() => (value, None),
        Ok(_) => (serde_json::Value::Null, Some(raw.to_string())),
        Err(_) => (serde_json::Value::Null, Some(raw.to_string())),
    }
}

fn tool_call_reference(name: &str, arguments: &serde_json::Value, raw: Option<&str>) -> ToolRef {
    let reference = tool_ref(name, arguments);
    if !reference.summary.is_empty() || raw.is_none() {
        return reference;
    }
    // Valid tool, undecodable arguments: retain the raw text so the call
    // still anchors its run instead of collapsing to an empty summary.
    ToolRef {
        summary: format!(
            "{name} {}",
            raw.unwrap_or_default()
                .chars()
                .take(200)
                .collect::<String>()
        ),
        files: Vec::new(),
        command: None,
    }
}

/// Groups sealed records by root `run_id`. Only `payload.kind == "run"`
/// records participate; tasks without a kind, reconciliation records, and
/// unknown payloads are skipped by construction. Payload schema versions
/// outside 1 through 3 are skipped for forward compatibility.
fn group_runs(records: &[MuseRecord]) -> Vec<RunAccumulator> {
    let mut runs: HashMap<String, RunAccumulator> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for record in records {
        // Only committed stream events project to evidence. Reconciliation
        // and status records stay in the reader for provenance.
        if record.record_type != "event" {
            continue;
        }
        if !(1..=3).contains(&record.payload_schema_version) {
            continue;
        }
        let payload = &record.payload;
        if payload.get("kind").and_then(|v| v.as_str()) != Some("run") {
            continue;
        }
        let Some(run_id) = payload.get("run_id").and_then(|v| v.as_str()) else {
            continue;
        };
        if run_id.is_empty() {
            continue;
        }
        let accumulator = runs.entry(run_id.to_string()).or_insert_with(|| {
            order.push(run_id.to_string());
            RunAccumulator::new(run_id, record)
        });
        accumulator.touch(record);
        let Some(event) = payload.get("event") else {
            continue;
        };
        let kind = event.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "started" => {
                if let Some(prompt) = event.get("prompt").and_then(|v| v.as_str()) {
                    if !prompt.trim().is_empty() && accumulator.prompt.is_none() {
                        accumulator.prompt = Some(prompt.to_string());
                    }
                }
            }
            "assistant_message_committed" => {
                if let Some(text) = event.get("text").and_then(|v| v.as_str()) {
                    if !text.trim().is_empty() {
                        accumulator.items.push(RunItem::AssistantText {
                            text: text.to_string(),
                        });
                    }
                }
            }
            "assistant_tool_calls_committed" => {
                let calls = event
                    .get("tool_calls")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                for call in &calls {
                    let name = call
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let call_id = call
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if call_id.is_empty() {
                        continue;
                    }
                    let (arguments, arguments_raw) = match call.get("args") {
                        Some(serde_json::Value::String(raw)) => parse_call_arguments(raw),
                        Some(value) if value.is_object() => (value.clone(), None),
                        _ => (serde_json::Value::Null, None),
                    };
                    let reference =
                        tool_call_reference(&name, &arguments, arguments_raw.as_deref());
                    accumulator.items.push(RunItem::ToolCall {
                        call: MuseToolCall {
                            call_id,
                            name,
                            reference,
                        },
                    });
                }
            }
            "tool_result_batch_committed" => {
                let results = event
                    .get("results")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                for result in &results {
                    let Some(call_id) = result.get("tool_call_id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let Some(text) = result.get("text").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    accumulator
                        .results
                        .entry(call_id.to_string())
                        .or_insert_with(|| text.to_string());
                }
            }
            "reasoning_summary_committed" => {
                if let Some(text) = event.get("text").and_then(|v| v.as_str()) {
                    if !text.trim().is_empty() {
                        accumulator.items.push(RunItem::Reasoning {
                            text: text.to_string(),
                        });
                    }
                }
            }
            "model_completed" => {
                if let Some(model) = event.get("model").and_then(|v| v.as_str()) {
                    if !model.trim().is_empty() {
                        accumulator.model = Some(model.to_string());
                    }
                }
                if let Some(usage) = event.get("usage").and_then(|v| v.as_object()) {
                    for (key, value) in usage {
                        let amount = value
                            .as_i64()
                            .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()));
                        if let Some(amount) = amount {
                            *accumulator.usage.entry(key.clone()).or_insert(0) += amount;
                        }
                    }
                }
            }
            "terminal" => {
                accumulator.terminal = true;
            }
            _ => {}
        }
    }
    let mut runs: Vec<RunAccumulator> = order
        .into_iter()
        .filter_map(|run_id| runs.remove(&run_id))
        .collect();
    runs.sort_by(|a, b| {
        a.start_sequence
            .cmp(&b.start_sequence)
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    runs
}

/// Derives the structured outcome for one call. Raw result bodies never
/// enter evidence; only the extracted command, exit status, duration, and
/// test counts survive. Non-bash calls carry no outcome, matching the Pi
/// policy that only command executions report outcomes.
fn outcome_for_call(call: &MuseToolCall, results: &HashMap<String, String>) -> Option<ToolOutcome> {
    if !call.name.eq_ignore_ascii_case("bash") {
        return None;
    }
    let result_text = results.get(&call.call_id);
    let command = call
        .reference
        .command
        .clone()
        .filter(|command| !command.trim().is_empty());
    match (command, result_text) {
        (Some(command), Some(text)) => {
            let mut outcome = ToolOutcome::unknown(command);
            upgrade_outcome_from_text(text, &mut outcome);
            Some(outcome)
        }
        (Some(command), None) => Some(ToolOutcome::unknown(command)),
        (None, Some(text)) => {
            // The executed command echoes inside bash result bodies.
            let echoed = serde_json::from_str::<serde_json::Value>(text)
                .ok()
                .and_then(|value| {
                    value
                        .get("command")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                });
            echoed.map(|command| {
                let mut outcome = ToolOutcome::unknown(command);
                upgrade_outcome_from_text(text, &mut outcome);
                outcome
            })
        }
        (None, None) => None,
    }
}

/// Evidence body for one run: user prompt, committed assistant text in
/// sequence order, compact tool-call summaries with structured bash
/// outcomes, and reasoning summaries that add non-duplicate text.
fn run_body(
    run: &RunAccumulator,
) -> Option<(String, Vec<String>, Vec<String>, Vec<serde_json::Value>)> {
    let prompt = run.prompt.as_deref().filter(|p| !p.trim().is_empty())?;
    let mut text = String::new();
    text.push_str("User:\n");
    text.push_str(prompt);
    text.push('\n');
    let mut files: Vec<String> = Vec::new();
    let mut commands: Vec<String> = Vec::new();
    let mut outcomes: Vec<serde_json::Value> = Vec::new();
    let mut assistant_joined = String::new();
    let mut seen_reasoning: Vec<String> = Vec::new();
    for item in &run.items {
        match item {
            RunItem::AssistantText { text: body } => {
                text.push_str("\nAssistant:\n");
                text.push_str(body);
                text.push('\n');
                assistant_joined.push_str(body);
                assistant_joined.push('\n');
            }
            RunItem::ToolCall { call } => {
                if call.reference.summary.trim().is_empty() {
                    continue;
                }
                text.push_str("\nTool: ");
                text.push_str(&call.reference.summary);
                text.push('\n');
                for file in &call.reference.files {
                    if !files.contains(file) {
                        files.push(file.clone());
                    }
                }
                if let Some(outcome) = outcome_for_call(call, &run.results) {
                    text.push_str("\nCommand: ");
                    text.push_str(&outcome.command);
                    text.push_str("\nOutcome: ");
                    text.push_str(&outcome.outcome);
                    text.push('\n');
                    commands.push(outcome.command.clone());
                    outcomes.push(outcome.to_json());
                }
            }
            RunItem::Reasoning { text: summary } => {
                let trimmed = summary.trim();
                if trimmed.is_empty()
                    || seen_reasoning.iter().any(|seen| seen == trimmed)
                    || assistant_joined.contains(trimmed)
                {
                    continue;
                }
                seen_reasoning.push(trimmed.to_string());
                text.push_str("\nReasoning:\n");
                text.push_str(trimmed);
                text.push('\n');
            }
        }
    }
    files.truncate(24);
    commands.truncate(16);
    outcomes.truncate(16);
    Some((text, files, commands, outcomes))
}

fn option_value(value: &Option<String>) -> serde_json::Value {
    match value {
        Some(value) => serde_json::json!(value),
        None => serde_json::Value::Null,
    }
}

fn option_int(value: Option<i64>) -> serde_json::Value {
    match value {
        Some(value) => serde_json::json!(value),
        None => serde_json::Value::Null,
    }
}

/// Persisted source metadata for one Muse session.
pub fn source_metadata(
    session: &MuseSession,
    sealed_end_sequence: Option<u64>,
) -> serde_json::Value {
    serde_json::json!({
        "harness": "muse",
        "session": session.session_id,
        "session_name": option_value(&session.session_name),
        "workspace_root": option_value(&session.workspace_root),
        "model_id": option_value(&session.model_id),
        "created_at_us": option_int(session.created_at_us),
        "updated_at_us": option_int(session.updated_at_us),
        "source_fingerprint": option_value(&session.source_fingerprint),
        "sealed_end_sequence": option_int(sealed_end_sequence.map(|v| v as i64)),
        "policy_version": MUSE_POLICY_VERSION,
    })
}

fn build_run_units(
    locator: &str,
    session_id: &str,
    episode: usize,
    run: &RunAccumulator,
    body: &str,
    files: &[String],
    commands: &[String],
    outcomes: &[serde_json::Value],
) -> Vec<BuiltUnit> {
    let breadcrumb = format!("{locator} > episode {episode}");
    let pieces = split_episode_pieces(&breadcrumb, body);
    let total = pieces.len();
    let usage = serde_json::Value::Object(
        run.usage
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::json!(value)))
            .collect(),
    );
    pieces
        .iter()
        .enumerate()
        .map(|(offset, piece)| {
            let piece_ordinal = offset + 1;
            // Every piece must remain within the unit token budget.
            debug_assert!(estimate_tokens(piece) <= MAX_TOKENS || total == 1);
            let evidence = if total == 1 {
                format!("{breadcrumb}\n\n{piece}")
            } else {
                format!("{breadcrumb} > piece {piece_ordinal}\n\n{piece}")
            };
            let routing = format!(
                "source: agent_episode\nsession: {session_id}\nepisode: {episode}\nrun: {}\n",
                run.run_id
            );
            // Stable identity comes from the run id: later sealed runs
            // append after it without renumbering this episode's hash.
            let content_hash = hash_segments(&[
                MUSE_POLICY_VERSION,
                locator,
                &run.run_id,
                &piece_ordinal.to_string(),
                &evidence,
                &routing,
            ]);
            let mut metadata = serde_json::json!({
                "harness": "muse",
                "session": session_id,
                "episode": episode,
                "run_id": run.run_id,
                "piece": piece_ordinal,
                "pieces": total,
                "sequence_start": run.start_sequence,
                "sequence_end": run.end_sequence,
                "record_id_start": run.start_record_id,
                "record_id_end": run.end_record_id,
                "files": files,
                "commands": commands,
                "outcomes": outcomes,
                "model": option_value(&run.model),
                "aggregate_usage": usage,
                "policy_version": MUSE_POLICY_VERSION,
            });
            crate::metadata::timestamp::set(&mut metadata, run.timestamp_secs);
            let mut anchors = vec![BuiltAnchor {
                kind: AnchorKind::Session,
                value: session_id.to_string(),
                relationship: "part_of".to_string(),
            }];
            for file in files {
                anchors.push(BuiltAnchor {
                    kind: AnchorKind::File,
                    value: file.clone(),
                    relationship: "touched".to_string(),
                });
            }
            BuiltUnit {
                kind: UnitKind::Episode,
                token_count: estimate_tokens(&evidence),
                content_hash,
                evidence_text: evidence,
                routing_text: routing,
                metadata,
                anchors,
            }
        })
        .collect()
}

/// Reduces the sealed log prefix to episode units: one unit per completed
/// root run with a nonempty prompt. Unterminated runs and prompt-less runs
/// contribute nothing.
pub fn sealed_units(log: &ParsedLog, session: &MuseSession) -> Vec<BuiltUnit> {
    let locator = muse_session_locator(&session.session_id);
    let mut runs = group_runs(sealed_records(log));
    if runs.len() > MAX_EPISODES_PER_SESSION {
        let excess = runs.len() - MAX_EPISODES_PER_SESSION;
        runs.drain(..excess);
    }
    let mut units = Vec::new();
    let mut episode = 0usize;
    for run in &runs {
        // A run terminal boundary is required before emission: the live
        // tail of an active run never enters repository memory.
        if !run.terminal {
            continue;
        }
        let Some((body, files, commands, outcomes)) = run_body(run) else {
            continue;
        };
        episode += 1;
        units.extend(build_run_units(
            &locator,
            &session.session_id,
            episode,
            run,
            &body,
            &files,
            &commands,
            &outcomes,
        ));
    }
    units
}

/// Ingests one Muse transcript: decodes the log, selects the sealed
/// prefix, and reduces completed runs to episode units. Sessions without
/// a durable end marker contribute no units.
pub fn ingest_muse_session(
    content: &str,
    session: &MuseSession,
) -> Result<Vec<BuiltUnit>, Box<dyn std::error::Error + Send + Sync>> {
    let log = read_muse_log(content)?;
    Ok(sealed_units(&log, session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::UnitKind;
    use crate::ingest::units::MAX_TOKENS;

    const SESSION_ID: &str = "sess-root-0001";
    const BASE_US: i64 = 1_700_000_000_000_000;

    fn test_session() -> MuseSession {
        MuseSession {
            session_id: SESSION_ID.to_string(),
            log_path: PathBuf::from("/tmp/muse-test-session/session.jsonl"),
            session_name: Some("test-name".to_string()),
            workspace_root: Some("/tmp/repo".to_string()),
            model_id: Some("muse-spark-test".to_string()),
            created_at_us: Some(BASE_US),
            updated_at_us: Some(BASE_US + 5_000_000),
            source_fingerprint: Some("fp-1".to_string()),
        }
    }

    fn rec(
        sequence: u64,
        payload_type: &str,
        payload_schema_version: u32,
        payload: serde_json::Value,
    ) -> String {
        rec_full(
            sequence,
            payload_type,
            payload_schema_version,
            payload,
            "event",
            "durable",
        )
    }

    fn rec_full(
        sequence: u64,
        payload_type: &str,
        payload_schema_version: u32,
        payload: serde_json::Value,
        record_type: &str,
        durability: &str,
    ) -> String {
        serde_json::json!({
            "schema_version": 1,
            "id": format!("rec-{sequence}"),
            "stream": {"kind": "session", "id": SESSION_ID},
            "sequence": sequence,
            "recorded_at": BASE_US + sequence as i64 * 1_000,
            "record_type": record_type,
            "durability": durability,
            "causation_id": null,
            "payload_type": payload_type,
            "payload_schema_version": payload_schema_version,
            "payload": payload,
        })
        .to_string()
    }

    fn run_payload(run_id: &str, event: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"kind": "run", "run_id": run_id, "event": event})
    }

    fn started(run_id: &str, prompt: &str) -> serde_json::Value {
        run_payload(
            run_id,
            serde_json::json!({"kind": "started", "prompt": prompt}),
        )
    }

    fn assistant(run_id: &str, text: &str) -> serde_json::Value {
        run_payload(
            run_id,
            serde_json::json!({"kind": "assistant_message_committed", "text": text}),
        )
    }

    fn calls(run_id: &str, message_id: &str, calls: &[(&str, &str, &str)]) -> serde_json::Value {
        let tool_calls: Vec<serde_json::Value> = calls
            .iter()
            .map(|(call_id, name, args)| {
                serde_json::json!({"call_id": call_id, "name": name, "args": args})
            })
            .collect();
        run_payload(
            run_id,
            serde_json::json!({
                "kind": "assistant_tool_calls_committed",
                "message_id": message_id,
                "tool_calls": tool_calls,
            }),
        )
    }

    fn results(run_id: &str, batch_id: &str, results: &[(&str, &str)]) -> serde_json::Value {
        let entries: Vec<serde_json::Value> = results
            .iter()
            .map(|(call_id, text)| serde_json::json!({"tool_call_id": call_id, "text": text}))
            .collect();
        run_payload(
            run_id,
            serde_json::json!({
                "kind": "tool_result_batch_committed",
                "batch_id": batch_id,
                "results": entries,
            }),
        )
    }

    fn reasoning(run_id: &str, text: &str) -> serde_json::Value {
        run_payload(
            run_id,
            serde_json::json!({"kind": "reasoning_summary_committed", "text": text}),
        )
    }

    fn completed(run_id: &str, usage: serde_json::Value) -> serde_json::Value {
        run_payload(
            run_id,
            serde_json::json!({
                "kind": "model_completed",
                "model": "muse-spark-test",
                "usage": usage,
            }),
        )
    }

    fn terminal(run_id: &str) -> serde_json::Value {
        run_payload(
            run_id,
            serde_json::json!({"kind": "terminal", "terminal": "completed"}),
        )
    }

    fn session_end(sequence: u64) -> String {
        rec(
            sequence,
            "session.end",
            1,
            serde_json::json!({
                "kind": "session_end",
                "record": {"session_id": SESSION_ID, "exit_reason": "complete"},
            }),
        )
    }

    fn frame(children: &[(u64, String)]) -> String {
        let encoded: Vec<serde_json::Value> = children
            .iter()
            .enumerate()
            .map(
                |(index, (_, line))| serde_json::json!({"child_index": index, "record_json": line}),
            )
            .collect();
        serde_json::json!({
            "retained_frame": "session_permission_transaction",
            "frame_schema_version": 1,
            "outer_log_ordinal": 1,
            "transaction_id": "tx-1",
            "children": encoded,
        })
        .to_string()
    }

    fn completed_run_lines(
        run_id: &str,
        start: u64,
        prompt: &str,
        assistant_text: &str,
    ) -> Vec<String> {
        vec![
            rec(start, "runtime.session", 1, started(run_id, prompt)),
            rec(
                start + 1,
                "runtime.session",
                1,
                assistant(run_id, assistant_text),
            ),
            rec(start + 2, "runtime.session", 1, terminal(run_id)),
        ]
    }

    #[test]
    fn plain_records_and_frame_children_decode_with_provenance() {
        let child_a = rec(
            1,
            "runtime.session.permission_format_declared",
            1,
            serde_json::json!({}),
        );
        let child_b = rec(
            2,
            "runtime.session.permission_profile_committed",
            1,
            serde_json::json!({}),
        );
        let content = format!(
            "{}\n{}\n",
            frame(&[(0, child_a), (1, child_b)]),
            rec(3, "session.end", 1, serde_json::json!({}))
        );
        let log = read_muse_log(&content).unwrap();
        assert_eq!(log.records.len(), 3);
        assert_eq!(log.records[0].sequence, 1);
        assert_eq!(log.records[0].log_ordinal, 1);
        assert_eq!(log.records[0].frame_child_index, Some(0));
        assert_eq!(log.records[1].frame_child_index, Some(1));
        assert_eq!(log.records[2].frame_child_index, None);
        assert_eq!(log.records[2].log_ordinal, 2);
    }

    #[test]
    fn child_sequence_orders_the_stream_not_outer_ordinal() {
        // Frame at physical ordinal 1 holds sequences 2 then 1.
        let child_seq2 = rec(2, "runtime.session", 1, serde_json::json!({"kind": "run"}));
        let child_seq1 = rec(1, "runtime.session", 1, serde_json::json!({"kind": "run"}));
        let content = format!(
            "{}\n{}\n",
            frame(&[(0, child_seq2), (1, child_seq1)]),
            rec(3, "session.end", 1, serde_json::json!({})),
        );
        let log = read_muse_log(&content).unwrap();
        let sequences: Vec<u64> = log.records.iter().map(|r| r.sequence).collect();
        assert_eq!(sequences, vec![1, 2, 3]);
        assert_eq!(log.records[0].frame_child_index, Some(1));
        assert_eq!(log.records[1].frame_child_index, Some(0));
    }

    #[test]
    fn omitted_markers_leave_gaps_without_evidence() {
        let content = format!(
            "{}\n{}\n{}\n{}\n",
            rec(5, "runtime.session", 1, started("run-1", "Do the thing")),
            r#"{"retained_marker":"omitted_live_only","position":{"id":"x","sequence":6},"omitted_record":{"payload_type":"runtime.session","omission_class":"task_tool_delta_v1"}}"#,
            rec(7, "runtime.session", 1, terminal("run-1")),
            session_end(8),
        );
        let log = read_muse_log(&content).unwrap();
        // The marker is a provenance gap: no record, no error.
        assert_eq!(log.records.len(), 3);
        let units = ingest_muse_session(&content, &test_session()).unwrap();
        assert_eq!(units.len(), 1);
        assert!(units[0].evidence_text.contains("Do the thing"));
    }

    #[test]
    fn payload_schema_versions_one_through_three_are_accepted() {
        let content = format!(
            "{}\n{}\n{}\n{}\n",
            rec(1, "runtime.session", 2, started("run-1", "Versioned run")),
            rec(
                2,
                "runtime.session",
                3,
                assistant("run-1", "Versioned answer.")
            ),
            rec(3, "runtime.session", 1, terminal("run-1")),
            session_end(4),
        );
        let units = ingest_muse_session(&content, &test_session()).unwrap();
        assert_eq!(units.len(), 1);
        assert!(units[0].evidence_text.contains("Versioned answer."));
    }

    #[test]
    fn unknown_payloads_skip_and_unknown_envelopes_reject_the_session() {
        let content = format!(
            "{}\n{}\n{}\n{}\n{}\n",
            rec(
                1,
                "runtime.session",
                1,
                started("run-1", "Future-proof run")
            ),
            rec(2, "future.thing", 1, serde_json::json!({"anything": true})),
            rec(
                3,
                "runtime.session",
                9,
                assistant("run-1", "Dropped by schema gate.")
            ),
            rec(4, "runtime.session", 1, terminal("run-1")),
            session_end(5),
        );
        // Unknown payload types and out-of-range payload schemas skip the
        // record while the known run still emits.
        let units = ingest_muse_session(&content, &test_session()).unwrap();
        assert_eq!(units.len(), 1);
        assert!(!units[0].evidence_text.contains("Dropped by schema gate."));

        let bad = format!(
            "{}\n",
            serde_json::json!({
                "schema_version": 2, "id": "r1",
                "stream": {"kind": "session", "id": SESSION_ID},
                "sequence": 1, "recorded_at": BASE_US,
                "record_type": "event", "durability": "durable",
                "payload_type": "runtime.session", "payload_schema_version": 1,
                "payload": {},
            }),
        );
        assert!(matches!(
            read_muse_log(&bad),
            Err(MuseReadError::UnsupportedEnvelope(2))
        ));
        assert!(ingest_muse_session(&bad, &test_session()).is_err());
    }

    #[test]
    fn task_records_without_kind_and_reconciliation_records_are_ignored() {
        let content = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n",
            rec(
                1,
                "runtime.session",
                1,
                started("run-1", "Kindless task run")
            ),
            // runtime.session.task shape with no payload.kind.
            rec(
                2,
                "runtime.session.task",
                1,
                serde_json::json!({
                    "task_id": "t-1",
                    "event": {"kind": "completed", "task_id": "t-1"},
                }),
            ),
            // Reconciliation must never become evidence.
            rec_full(
                3,
                "runtime.session",
                1,
                assistant("run-1", "RECONCILIATION MUST NOT INDEX"),
                "reconciliation",
                "durable",
            ),
            rec(4, "runtime.session", 1, assistant("run-1", "Real answer.")),
            rec(5, "runtime.session", 1, terminal("run-1")),
            session_end(6),
        );
        let units = ingest_muse_session(&content, &test_session()).unwrap();
        assert_eq!(units.len(), 1);
        assert!(!units[0]
            .evidence_text
            .contains("RECONCILIATION MUST NOT INDEX"));
        assert!(units[0].evidence_text.contains("Real answer."));
    }

    #[test]
    fn incomplete_trailing_line_is_ignored() {
        let mut content = format!(
            "{}\n{}\n{}\n",
            rec(1, "runtime.session", 1, started("run-1", "Torn tail run")),
            rec(2, "runtime.session", 1, terminal("run-1")),
            session_end(3),
        );
        // Actively appended torn write: no trailing newline, cut mid-object.
        content.push_str(r#"{"schema_version":1,"id":"rec-4","stream":{"kind":"session""#);
        let units = ingest_muse_session(&content, &test_session()).unwrap();
        assert_eq!(units.len(), 1);
    }

    #[test]
    fn microsecond_timestamps_convert_to_seconds() {
        let line = serde_json::json!({
            "schema_version": 1, "id": "rec-1",
            "stream": {"kind": "session", "id": SESSION_ID},
            "sequence": 1, "recorded_at": 1_700_000_123_456_789_i64,
            "record_type": "event", "durability": "durable",
            "causation_id": null,
            "payload_type": "runtime.session", "payload_schema_version": 1,
            "payload": started("run-1", "Timed run"),
        })
        .to_string();
        let content = format!(
            "{line}\n{}\n{}\n",
            rec(2, "runtime.session", 1, terminal("run-1")),
            session_end(3),
        );
        let units = ingest_muse_session(&content, &test_session()).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].metadata["timestamp"], 1_700_000_123);
    }

    #[test]
    fn one_completed_run_becomes_one_episode_unit() {
        let mut lines = completed_run_lines(
            "run-1",
            1,
            "Fix the token loop",
            "Fixed by rotating tokens.",
        );
        lines.push(session_end(4));
        let units = ingest_muse_session(&lines.join("\n"), &test_session()).unwrap();
        assert_eq!(units.len(), 1);
        let unit = &units[0];
        assert_eq!(unit.kind, UnitKind::Episode);
        assert!(unit.token_count <= MAX_TOKENS);
        assert_eq!(unit.metadata["harness"], "muse");
        assert_eq!(unit.metadata["session"], SESSION_ID);
        assert_eq!(unit.metadata["episode"], 1);
        assert_eq!(unit.metadata["run_id"], "run-1");
        assert_eq!(unit.metadata["piece"], 1);
        assert_eq!(unit.metadata["pieces"], 1);
        assert_eq!(unit.metadata["policy_version"], MUSE_POLICY_VERSION);
        assert_eq!(unit.metadata["sequence_start"], 1);
        assert_eq!(unit.metadata["sequence_end"], 3);
        assert_eq!(unit.metadata["record_id_start"], "rec-1");
        assert!(unit
            .evidence_text
            .starts_with("muse-session:sess-root-0001 > episode 1"));
        assert!(unit.evidence_text.contains("Fix the token loop"));
        assert!(unit.evidence_text.contains("Fixed by rotating tokens."));
        assert!(unit.routing_text.contains("source: agent_episode"));
        assert!(unit.routing_text.contains("run: run-1"));
        assert!(unit
            .anchors
            .iter()
            .any(|a| a.kind == crate::core::AnchorKind::Session && a.value == SESSION_ID));
        assert_eq!(
            muse_session_locator(SESSION_ID),
            "muse-session:sess-root-0001"
        );
    }

    #[test]
    fn multiple_completed_runs_emit_ordered_episodes() {
        let mut lines = completed_run_lines("run-b", 1, "Second prompt", "Second answer.");
        lines.extend(completed_run_lines(
            "run-a",
            10,
            "First prompt",
            "First answer.",
        ));
        lines.push(session_end(13));
        // Runs order by start sequence even when the run ids sort otherwise.
        let units = ingest_muse_session(&lines.join("\n"), &test_session()).unwrap();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].metadata["run_id"], "run-b");
        assert_eq!(units[0].metadata["episode"], 1);
        assert_eq!(units[1].metadata["run_id"], "run-a");
        assert_eq!(units[1].metadata["episode"], 2);
    }

    #[test]
    fn unterminated_runs_and_post_end_records_are_excluded() {
        let content = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n",
            rec(
                1,
                "runtime.session",
                1,
                started("run-live", "Still working")
            ),
            rec(
                2,
                "runtime.session",
                1,
                assistant("run-live", "Partial answer.")
            ),
            rec(
                3,
                "runtime.session",
                1,
                started("run-done", "Sealed prompt")
            ),
            rec(4, "runtime.session", 1, terminal("run-done")),
            session_end(5),
            // Live tail after the end marker never enters the index.
            rec(
                6,
                "runtime.session",
                1,
                started("run-after", "After the end")
            ),
        );
        let log = read_muse_log(&content).unwrap();
        assert_eq!(log.sealed_end_sequence, Some(5));
        let units = ingest_muse_session(&content, &test_session()).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].metadata["run_id"], "run-done");
    }

    #[test]
    fn session_without_durable_end_contributes_no_units() {
        let content = format!(
            "{}\n{}\n",
            rec(1, "runtime.session", 1, started("run-1", "Live prompt")),
            rec(2, "runtime.session", 1, terminal("run-1")),
        );
        let log = read_muse_log(&content).unwrap();
        assert_eq!(log.sealed_len, None);
        assert!(ingest_muse_session(&content, &test_session())
            .unwrap()
            .is_empty());
        // An ephemeral end marker does not seal either.
        let ephemeral = format!(
            "{content}{}\n",
            rec_full(
                3,
                "session.end",
                1,
                serde_json::json!({"kind": "session_end", "record": {}}),
                "event",
                "ephemeral",
            ),
        );
        assert!(ingest_muse_session(&ephemeral, &test_session())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn parallel_calls_join_results_by_call_id() {
        let read_args = serde_json::json!({"path": "src/auth.rs"}).to_string();
        let bash_args = serde_json::json!({"command": "cargo test auth"}).to_string();
        let edit_args = serde_json::json!({"path": "src/auth.rs"}).to_string();
        let bash_result = serde_json::json!({
            "chunk_id": "exec-1",
            "command": "cargo test auth",
            "exit_code": 1,
            "output": "FAILED ...",
        })
        .to_string();
        let content = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
            rec(
                1,
                "runtime.session",
                1,
                started("run-1", "Check the auth module")
            ),
            rec(
                2,
                "runtime.session",
                1,
                calls(
                    "run-1",
                    "msg-1",
                    &[
                        ("c1", "read_file", &read_args),
                        ("c2", "bash", &bash_args),
                        ("c3", "edit_file", &edit_args),
                    ],
                ),
            ),
            rec(
                3,
                "runtime.session",
                1,
                results(
                    "run-1",
                    "msg-1",
                    &[
                        (
                            "c1",
                            "900 lines of file contents that must not become evidence"
                        ),
                        ("c2", &bash_result),
                        ("c3", "Edited src/auth.rs successfully"),
                    ],
                ),
            ),
            rec(
                4,
                "runtime.session",
                1,
                assistant("run-1", "Auth now rotates tokens.")
            ),
            rec(
                5,
                "runtime.session",
                1,
                completed(
                    "run-1",
                    serde_json::json!({"input_tokens": 100, "output_tokens": 20})
                ),
            ),
            rec(6, "runtime.session", 1, terminal("run-1")),
            session_end(7),
        );
        let units = ingest_muse_session(&content, &test_session()).unwrap();
        assert_eq!(units.len(), 1);
        let unit = &units[0];
        assert!(unit.evidence_text.contains("Tool: read_file src/auth.rs"));
        assert!(unit.evidence_text.contains("Tool: edit_file src/auth.rs"));
        assert!(unit.evidence_text.contains("Command: cargo test auth"));
        assert!(unit.evidence_text.contains("Outcome: failed"));
        assert!(!unit.evidence_text.contains("900 lines of file contents"));
        assert!(!unit.evidence_text.contains("FAILED ..."));
        let files = unit.metadata["files"].as_array().unwrap();
        assert!(files.iter().any(|v| v == "src/auth.rs"));
        let commands = unit.metadata["commands"].as_array().unwrap();
        assert!(commands.iter().any(|v| v == "cargo test auth"));
        let outcomes = unit.metadata["outcomes"].as_array().unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0]["exit_code"], 1);
        assert_eq!(outcomes[0]["outcome"], "failed");
        assert_eq!(unit.metadata["aggregate_usage"]["input_tokens"], 100);
        assert_eq!(unit.metadata["aggregate_usage"]["output_tokens"], 20);
        assert_eq!(unit.metadata["model"], "muse-spark-test");
    }

    #[test]
    fn malformed_json_string_arguments_keep_a_raw_fallback() {
        let content = format!(
            "{}\n{}\n{}\n{}\n",
            rec(1, "runtime.session", 1, started("run-1", "Weird args run")),
            rec(
                2,
                "runtime.session",
                1,
                calls("run-1", "msg-1", &[("c1", "read_file", "{not-json")]),
            ),
            rec(3, "runtime.session", 1, terminal("run-1")),
            session_end(4),
        );
        let units = ingest_muse_session(&content, &test_session()).unwrap();
        assert_eq!(units.len(), 1);
        assert!(units[0].evidence_text.contains("read_file"));
        assert!(units[0].evidence_text.contains("{not-json"));
    }

    #[test]
    fn reasoning_summaries_enter_only_when_non_duplicate() {
        let content = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n",
            rec(1, "runtime.session", 1, started("run-1", "Reasoned run")),
            rec(
                2,
                "runtime.session",
                1,
                assistant("run-1", "The fix is token rotation.")
            ),
            rec(
                3,
                "runtime.session",
                1,
                reasoning("run-1", "The fix is token rotation.")
            ),
            rec(
                4,
                "runtime.session",
                1,
                reasoning("run-1", "Also check the refresh path.")
            ),
            rec(5, "runtime.session", 1, terminal("run-1")),
            session_end(6),
        );
        let units = ingest_muse_session(&content, &test_session()).unwrap();
        assert_eq!(units.len(), 1);
        // Duplicate of the assistant text stays out; the novel summary stays in, once.
        assert_eq!(
            units[0]
                .evidence_text
                .matches("The fix is token rotation.")
                .count(),
            1
        );
        assert!(units[0]
            .evidence_text
            .contains("Also check the refresh path."));
    }

    #[test]
    fn oversized_episodes_split_within_budget() {
        let long = "Walk the auth module top down and explain every validation step. ".repeat(120);
        let content = format!(
            "{}\n{}\n{}\n{}\n",
            rec(
                1,
                "runtime.session",
                1,
                started("run-1", "Explain the module")
            ),
            rec(2, "runtime.session", 1, assistant("run-1", &long)),
            rec(3, "runtime.session", 1, terminal("run-1")),
            session_end(4),
        );
        let units = ingest_muse_session(&content, &test_session()).unwrap();
        assert!(units.len() > 1, "long run must split: {}", units.len());
        for (offset, unit) in units.iter().enumerate() {
            assert!(unit.token_count <= MAX_TOKENS, "piece {offset}");
            assert_eq!(unit.metadata["piece"], (offset + 1) as u64);
            assert_eq!(unit.metadata["pieces"], units.len() as u64);
            assert_eq!(unit.metadata["run_id"], "run-1");
            assert!(unit
                .evidence_text
                .contains(&format!("piece {}", offset + 1)));
        }
        let hashes: std::collections::HashSet<_> =
            units.iter().map(|unit| unit.content_hash.clone()).collect();
        assert_eq!(hashes.len(), units.len(), "pieces hash distinctly");
    }

    #[test]
    fn stable_hashes_survive_appending_a_later_sealed_run() {
        let mut first: Vec<String> =
            completed_run_lines("run-1", 1, "First prompt", "First answer.");
        first.push(session_end(4));
        let before = ingest_muse_session(&first.join("\n"), &test_session()).unwrap();
        assert_eq!(before.len(), 1);

        let mut later: Vec<String> =
            completed_run_lines("run-1", 1, "First prompt", "First answer.");
        later.extend(completed_run_lines(
            "run-2",
            10,
            "Second prompt",
            "Second answer.",
        ));
        later.push(session_end(13));
        let after = ingest_muse_session(&later.join("\n"), &test_session()).unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].content_hash, before[0].content_hash);
        assert_ne!(after[1].content_hash, before[0].content_hash);
    }

    #[test]
    fn source_metadata_carries_index_attribution() {
        let session = test_session();
        let metadata = source_metadata(&session, Some(42));
        assert_eq!(metadata["harness"], "muse");
        assert_eq!(metadata["session"], SESSION_ID);
        assert_eq!(metadata["session_name"], "test-name");
        assert_eq!(metadata["workspace_root"], "/tmp/repo");
        assert_eq!(metadata["model_id"], "muse-spark-test");
        assert_eq!(metadata["created_at_us"], BASE_US);
        assert_eq!(metadata["sealed_end_sequence"], 42);
        assert_eq!(metadata["policy_version"], MUSE_POLICY_VERSION);
    }
}
