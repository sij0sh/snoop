//! JSONL parsing for pi agent session files: raw lines become episode turns.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde::Deserialize;

use super::tools::{tool_ref, ToolRef};

#[derive(Debug, Deserialize)]
pub(super) struct SessionEntry {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(default)]
    message: Option<SessionMessage>,
}

#[derive(Debug, Deserialize)]
struct SessionMessage {
    role: String,
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default, rename = "toolCallId")]
    tool_call_id: Option<String>,
    #[serde(default, rename = "toolName")]
    tool_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<serde_json::Value>,
    #[serde(default, rename = "toolCallId")]
    tool_call_id: Option<String>,
    #[serde(default, rename = "tool_use_id")]
    tool_use_id: Option<String>,
    #[serde(default, rename = "toolName")]
    tool_name: Option<String>,
}

pub(super) fn parse_timestamp(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let year: i64 = value.get(0..4)?.parse().ok()?;
    let month: i64 = value.get(5..7)?.parse().ok()?;
    let day: i64 = value.get(8..10)?.parse().ok()?;
    let hour: i64 = value.get(11..13)?.parse().ok()?;
    let minute: i64 = value.get(14..16)?.parse().ok()?;
    let second: i64 = value.get(17..19)?.parse().ok()?;

    let years = if month <= 2 { year - 1 } else { year };
    let era = if years >= 0 { years } else { years - 399 } / 400;
    let year_of_era = years - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EventKind {
    UserText,
    AssistantText,
    ToolCall,
    ToolResult,
}

#[derive(Debug, Clone)]
pub(super) struct EpisodeEvent {
    pub(super) id: String,
    pub(super) kind: EventKind,
    pub(super) start_byte: usize,
    pub(super) end_byte: usize,
    pub(super) text: String,
    pub(super) files: Vec<String>,
    pub(super) outcome: Option<BashOutcome>,
}

#[derive(Debug, Clone)]
pub(super) struct EpisodeTurn {
    pub(super) absolute_index: usize,
    pub(super) timestamp: Option<i64>,
    pub(super) events: Vec<EpisodeEvent>,
}

impl EpisodeTurn {
    pub(super) fn user_text(&self) -> &str {
        self.events
            .iter()
            .find(|event| event.kind == EventKind::UserText)
            .map(|event| event.text.as_str())
            .unwrap_or_default()
    }

    pub(super) fn start_key(&self) -> &str {
        self.events
            .first()
            .map(|event| event.id.as_str())
            .unwrap_or("0")
    }

    pub(super) fn files(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        self.events
            .iter()
            .flat_map(|event| event.files.iter().cloned())
            .filter(|file| seen.insert(file.clone()))
            .take(24)
            .collect()
    }

    pub(super) fn commands(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|event| event.outcome.as_ref().map(|o| o.command.clone()))
            .take(16)
            .collect()
    }

    pub(super) fn outcomes(&self) -> Vec<serde_json::Value> {
        self.events
            .iter()
            .filter_map(|event| event.outcome.as_ref())
            .take(16)
            .map(BashOutcome::to_json)
            .collect()
    }

    /// Evidence body for the turn: user and assistant text, tool-call
    /// summaries, and structured bash outcomes. Raw tool results never enter.
    pub(super) fn body(&self) -> String {
        let mut text = String::new();
        for event in &self.events {
            match event.kind {
                EventKind::UserText => {
                    text.push_str("User:\n");
                    text.push_str(&event.text);
                    text.push('\n');
                }
                EventKind::AssistantText => {
                    text.push_str("\nAssistant:\n");
                    text.push_str(&event.text);
                    text.push('\n');
                }
                EventKind::ToolCall => {
                    text.push_str("\nTool: ");
                    text.push_str(&event.text);
                    text.push('\n');
                }
                EventKind::ToolResult => {
                    if let Some(outcome) = &event.outcome {
                        text.push_str("\nCommand: ");
                        text.push_str(&outcome.command);
                        text.push_str("\nOutcome: ");
                        text.push_str(&outcome.outcome);
                        text.push('\n');
                    }
                }
            }
        }
        text
    }

    pub(super) fn byte_range(&self) -> (usize, usize) {
        match (self.events.first(), self.events.last()) {
            (Some(first), Some(last)) => (first.start_byte, last.end_byte),
            _ => (0, 0),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct BashOutcome {
    command: String,
    outcome: String,
    exit_code: Option<i64>,
    duration_ms: Option<i64>,
    test_counts: Option<serde_json::Value>,
}

impl BashOutcome {
    fn unknown(command: String) -> Self {
        Self {
            command: command.chars().take(200).collect(),
            outcome: "unknown".to_string(),
            exit_code: None,
            duration_ms: None,
            test_counts: None,
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "command": self.command,
            "outcome": self.outcome,
        });
        if let Some(exit_code) = self.exit_code {
            value["exit_code"] = serde_json::json!(exit_code);
        }
        if let Some(duration_ms) = self.duration_ms {
            value["duration_ms"] = serde_json::json!(duration_ms);
        }
        if let Some(test_counts) = &self.test_counts {
            value["test_counts"] = test_counts.clone();
        }
        value
    }
}

fn text_of(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter(|block| block.kind == "text")
        .filter_map(|block| block.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

pub(super) fn read_session_id(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(file).read_line(&mut first).ok()?;
    let header: serde_json::Value = serde_json::from_str(first.trim()).ok()?;
    let id = header.get("id")?.as_str()?.to_string();
    (!id.is_empty()).then_some(id)
}

fn block_call_id(block: &ContentBlock) -> Option<String> {
    block
        .id
        .clone()
        .or_else(|| block.tool_call_id.clone())
        .or_else(|| block.tool_use_id.clone())
}

fn block_result_id(block: &ContentBlock) -> Option<String> {
    block
        .tool_call_id
        .clone()
        .or_else(|| block.tool_use_id.clone())
}

fn outcome_from_payload(raw: &str, outcome: &mut BashOutcome) {
    let trimmed = raw.trim();
    if !trimmed.starts_with('{') {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return;
    };
    let exit = value
        .get("exitCode")
        .or_else(|| value.get("exit_code"))
        .and_then(|value| value.as_i64());
    let duration = value
        .get("durationMs")
        .or_else(|| value.get("duration_ms"))
        .and_then(|value| value.as_i64());
    let counts = ["testsPassed", "tests_passed", "passed", "failed", "total"]
        .iter()
        .filter_map(|key| {
            value
                .get(key)
                .and_then(|value| value.as_u64())
                .map(|v| (key, v))
        })
        .collect::<Vec<_>>();
    if exit.is_none() && duration.is_none() && counts.is_empty() {
        return;
    }
    if let Some(exit) = exit {
        outcome.exit_code = Some(exit);
        outcome.outcome = if exit == 0 {
            "passed".to_string()
        } else {
            "failed".to_string()
        };
    }
    if let Some(duration) = duration {
        outcome.duration_ms = Some(duration);
    }
    if !counts.is_empty() {
        let mut counts_json = serde_json::Map::new();
        for (key, value) in counts {
            counts_json.insert((*key).to_string(), serde_json::json!(value));
        }
        outcome.test_counts = Some(serde_json::Value::Object(counts_json));
    }
}

fn bash_outcome_from_content(
    blocks: &[ContentBlock],
    command: Option<String>,
) -> Option<BashOutcome> {
    let mut outcome = BashOutcome::unknown(command.unwrap_or_default());
    for block in blocks {
        let Some(raw) = block.text.as_deref() else {
            continue;
        };
        outcome_from_payload(raw, &mut outcome);
    }
    Some(outcome)
}

fn seal_turn(turns: &mut Vec<EpisodeTurn>, current: &mut Option<EpisodeTurn>) {
    if let Some(turn) = current.take() {
        if !turn.events.is_empty() {
            turns.push(turn);
        }
    }
}

/// Parses raw JSONL content into user-anchored episode turns. Tool results
/// are reduced to structured bash outcomes; their payloads are dropped.
pub(super) fn parse_pi_episodes(content: &str) -> Vec<EpisodeTurn> {
    let mut turns: Vec<EpisodeTurn> = Vec::new();
    let mut current: Option<EpisodeTurn> = None;
    let mut pending_bash: HashMap<String, String> = HashMap::new();
    let mut byte_offset = 0usize;

    for line in content.split_inclusive('\n') {
        let start_byte = byte_offset;
        let end_byte = start_byte + line.len();
        byte_offset = end_byte;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<SessionEntry>(line) else {
            continue;
        };
        if entry.kind != "message" {
            continue;
        }
        let Some(message) = &entry.message else {
            continue;
        };
        let timestamp = entry.timestamp.as_deref().and_then(parse_timestamp);
        let event_id = entry
            .id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| format!("b{start_byte}"));
        match message.role.as_str() {
            "user" => {
                let text = text_of(&message.content);
                if text.is_empty() {
                    continue;
                }
                seal_turn(&mut turns, &mut current);
                current = Some(EpisodeTurn {
                    absolute_index: turns.len(),
                    timestamp,
                    events: vec![EpisodeEvent {
                        id: event_id,
                        kind: EventKind::UserText,
                        start_byte,
                        end_byte,
                        text,
                        files: Vec::new(),
                        outcome: None,
                    }],
                });
            }
            "assistant" => {
                for block in message
                    .content
                    .iter()
                    .filter(|block| block.kind == "toolCall")
                {
                    if block.name.as_deref() == Some("bash") {
                        if let Some(call_id) = block_call_id(block) {
                            if let Some(command) = block
                                .arguments
                                .as_ref()
                                .and_then(|arguments| arguments["command"].as_str())
                                .map(|command| command.chars().take(200).collect::<String>())
                            {
                                pending_bash.insert(call_id, command);
                            }
                        }
                    }
                }
                let text = text_of(&message.content);
                let calls: Vec<(String, ToolRef)> = message
                    .content
                    .iter()
                    .filter(|block| block.kind == "toolCall")
                    .enumerate()
                    .map(|(position, block)| {
                        let name = block.name.as_deref().unwrap_or("unknown");
                        let arguments = block.arguments.clone().unwrap_or(serde_json::Value::Null);
                        let call_id = block_call_id(block)
                            .unwrap_or_else(|| format!("{event_id}:c{position}"));
                        (call_id, tool_ref(name, &arguments))
                    })
                    .collect();
                if text.is_empty() && calls.is_empty() {
                    continue;
                }
                if current.is_none() {
                    current = Some(EpisodeTurn {
                        absolute_index: turns.len(),
                        timestamp,
                        events: Vec::new(),
                    });
                }
                let turn = current.as_mut().unwrap();
                if !text.is_empty() {
                    turn.events.push(EpisodeEvent {
                        id: event_id.clone(),
                        kind: EventKind::AssistantText,
                        start_byte,
                        end_byte,
                        text,
                        files: Vec::new(),
                        outcome: None,
                    });
                }
                for (call_id, tool) in calls {
                    turn.events.push(EpisodeEvent {
                        id: call_id.clone(),
                        kind: EventKind::ToolCall,
                        start_byte,
                        end_byte,
                        text: tool.summary,
                        files: tool.files,
                        outcome: tool.command.map(|command| BashOutcome::unknown(command)),
                    });
                }
            }
            "toolResult" => {
                let call_id: Option<String> = message
                    .tool_call_id
                    .clone()
                    .or_else(|| message.content.iter().find_map(block_result_id));
                let command = call_id.as_deref().and_then(|id| pending_bash.remove(id));
                let is_bash = command.is_some()
                    || message.tool_name.as_deref() == Some("bash")
                    || message
                        .content
                        .iter()
                        .any(|block| block.tool_name.as_deref() == Some("bash"));
                let outcome = if is_bash {
                    bash_outcome_from_content(&message.content, command)
                } else {
                    None
                };
                if let Some(turn) = current.as_mut() {
                    turn.events.push(EpisodeEvent {
                        id: event_id,
                        kind: EventKind::ToolResult,
                        start_byte,
                        end_byte,
                        text: String::new(),
                        files: Vec::new(),
                        outcome,
                    });
                }
            }
            _ => {}
        }
    }
    seal_turn(&mut turns, &mut current);
    turns
}
