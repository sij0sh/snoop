use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::core::{
    hash_segments, AnchorKind, AtomKind, BuiltAnchor, BuiltUnit, ParsedAtom, UnitKind,
};
use crate::ingest::units::{
    estimate_tokens, split_oversized, MAX_TOKENS, SEGMENT_MIN_TOKENS, SEGMENT_TARGET_TOKENS,
};

pub const MAX_SESSION_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_EPISODES_PER_SESSION: usize = 200;

pub const SEGMENTATION_POLICY_VERSION: &str = "session-seg-v1";

const DEFAULT_SESSIONS_ROOT: &str = ".pi/agent/sessions";

pub const PI_HARNESS: HarnessLabel = HarnessLabel::new("pi-session");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessLabel {
    pub prefix: &'static str,
}

impl HarnessLabel {
    pub const fn new(prefix: &'static str) -> Self {
        Self { prefix }
    }

    pub fn locator(&self, session_id: &str) -> String {
        format!("{}:{}", self.prefix, session_id)
    }

    pub fn breadcrumb_root(&self, session_id: &str) -> String {
        format!("{}:{}", self.prefix, session_id)
    }
}

#[derive(Debug, Clone)]
pub struct SessionFile {
    pub path: PathBuf,
    pub session_id: String,
    pub harness: HarnessLabel,
}

pub fn session_directory_name(cwd: &str) -> String {
    format!(
        "--{}--",
        cwd.trim_start_matches(['/', '\\'])
            .replace(['/', '\\', ':'], "-")
    )
}

pub fn sessions_root(home: &Path) -> PathBuf {
    if let Some(override_root) = std::env::var_os("SNOOP_SESSIONS_ROOT") {
        return PathBuf::from(override_root);
    }
    home.join(DEFAULT_SESSIONS_ROOT)
}

fn directory_entries_matching(
    root: &Path,
    extension: &str,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
    let mut files = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some(extension) {
            continue;
        }
        if entry
            .metadata()
            .is_ok_and(|meta| meta.len() > MAX_SESSION_BYTES)
        {
            continue;
        }
        files.push(path);
    }
    files.sort();
    Ok(files)
}

pub fn discover_sessions(
    home: &Path,
    repo_root: &Path,
) -> Result<Vec<SessionFile>, Box<dyn std::error::Error + Send + Sync>> {
    let directory = sessions_root(home).join(session_directory_name(&repo_root.to_string_lossy()));
    let mut sessions = Vec::new();
    for path in directory_entries_matching(&directory, "jsonl")? {
        let Some(session_id) = read_session_id(&path) else {
            continue;
        };
        sessions.push(SessionFile {
            path,
            session_id,
            harness: PI_HARNESS,
        });
    }
    Ok(sessions)
}

#[derive(Debug, Deserialize)]
struct SessionEntry {
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

fn parse_timestamp(value: &str) -> Option<i64> {
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
enum EventKind {
    UserText,
    AssistantText,
    ToolCall,
    ToolResult,
}

impl EventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::UserText => "user_text",
            Self::AssistantText => "assistant_text",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
        }
    }
}

#[derive(Debug, Clone)]
struct EpisodeEvent {
    id: String,
    kind: EventKind,

    group: u32,
    start_byte: usize,
    end_byte: usize,
    text: String,
    tool: Option<String>,
    call_id: Option<String>,
    files: Vec<String>,
    command: Option<String>,
    outcome: Option<BashOutcome>,
}

impl EpisodeEvent {
    fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "id": self.id,
            "kind": self.kind.as_str(),
            "start_byte": self.start_byte,
            "end_byte": self.end_byte,
        });
        if let Some(tool) = &self.tool {
            value["tool"] = serde_json::json!(tool);
        }
        if let Some(call_id) = &self.call_id {
            value["call_id"] = serde_json::json!(call_id);
        }
        if !self.files.is_empty() {
            value["files"] = serde_json::json!(self.files);
        }
        if let Some(command) = &self.command {
            value["command"] = serde_json::json!(command);
        }
        if let Some(outcome) = &self.outcome {
            value["outcome"] = outcome.to_json();
        }
        value
    }
}

#[derive(Debug, Clone)]
struct EpisodeTurn {
    role: String,

    absolute_index: usize,
    timestamp: Option<i64>,
    events: Vec<EpisodeEvent>,
}

impl EpisodeTurn {
    fn user_text(&self) -> &str {
        self.events
            .iter()
            .find(|event| event.kind == EventKind::UserText)
            .map(|event| event.text.as_str())
            .unwrap_or_default()
    }

    fn start_key(&self) -> &str {
        self.events
            .first()
            .map(|event| event.id.as_str())
            .unwrap_or("0")
    }

    fn files(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        self.events
            .iter()
            .flat_map(|event| event.files.iter().cloned())
            .filter(|file| seen.insert(file.clone()))
            .take(24)
            .collect()
    }

    fn commands(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|event| event.command.clone())
            .take(16)
            .collect()
    }

    fn outcomes(&self) -> Vec<serde_json::Value> {
        self.events
            .iter()
            .filter_map(|event| event.outcome.as_ref())
            .take(16)
            .map(BashOutcome::to_json)
            .collect()
    }

    fn atom_text(&self) -> String {
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
                EventKind::ToolResult => {}
            }
        }
        if text.is_empty() {
            return text;
        }
        if !self
            .events
            .iter()
            .any(|event| event.kind == EventKind::UserText)
        {
            text = format!("(no user turn)\n{text}");
        }
        text
    }
}

#[derive(Debug, Clone)]
struct BashOutcome {
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

#[derive(Debug, Clone)]
struct ToolRef {
    tool: String,
    summary: String,
    files: Vec<String>,
    command: Option<String>,
}

fn extract_file(value: &str) -> Option<String> {
    let cleaned = value.trim().trim_matches('"').trim_matches('\'');
    if cleaned.is_empty() || cleaned.len() > 512 || cleaned.contains('\0') {
        return None;
    }
    if !cleaned.contains('/') && !cleaned.contains('.') && !cleaned.contains('\\') {
        return None;
    }
    if !cleaned.chars().next().is_some_and(|character| {
        character.is_alphanumeric() || character == '.' || character == '_' || character == '/'
    }) {
        return None;
    }
    let looks_like_path = cleaned.split(['/', '\\']).all(|segment| {
        segment.is_empty()
            || segment.chars().all(|character| {
                character.is_alphanumeric()
                    || matches!(
                        character,
                        '.' | '_' | '-' | ' ' | '@' | '+' | '(' | ')' | ','
                    )
            })
    });
    looks_like_path.then(|| cleaned.replace('\\', "/"))
}

fn tool_ref(name: &str, arguments: &serde_json::Value) -> ToolRef {
    let mut files = Vec::new();
    let mut command = None;
    let mut summary = String::new();
    let lowered = name.to_ascii_lowercase();
    match lowered.as_str() {
        "read" | "edit" | "write" => {
            let path = arguments["path"]
                .as_str()
                .or_else(|| arguments["file_path"].as_str());
            if let Some(path) = path {
                if let Some(file) = extract_file(path) {
                    files.push(file.clone());
                    summary = format!("{name} {file}");
                }
            }
        }
        "bash" => {
            if let Some(shell) = arguments["command"].as_str() {
                command = Some(shell.to_string());
                summary = format!("bash {}", shell.lines().next().unwrap_or(shell));
                for token in shell.split_whitespace() {
                    if let Some(file) = extract_file(token) {
                        files.push(file);
                    }
                    if files.len() >= 8 {
                        break;
                    }
                }
            }
        }
        _ => {
            summary = name.to_string();
        }
    }
    ToolRef {
        tool: name.to_string(),
        summary,
        files,
        command,
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

fn read_session_id(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(file).read_line(&mut first).ok()?;
    let header: serde_json::Value = serde_json::from_str(first.trim()).ok()?;
    let id = header.get("id")?.as_str()?.to_string();
    (!id.is_empty()).then_some(id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Orient,
    Investigate,
    Modify,
    Validate,
    Revise,
    Resolve,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Orient => "orient",
            Self::Investigate => "investigate",
            Self::Modify => "modify",
            Self::Validate => "validate",
            Self::Revise => "revise",
            Self::Resolve => "resolve",
        }
    }
}

const VALIDATE_COMMANDS: &[&str] = &[
    "cargo test",
    "cargo build",
    "cargo clippy",
    "cargo check",
    "cargo nextest",
    "npm test",
    "npm run test",
    "pnpm test",
    "yarn test",
    "pytest",
    "python -m pytest",
    "python3 -m pytest",
    "go test",
    "tsc",
    "eslint",
    "ruff",
    "mypy",
    "gradle test",
    "mvn test",
];

const MODIFY_COMMANDS: &[&str] = &[
    "git add",
    "git commit",
    "git checkout",
    "git restore",
    "git merge",
    "git rebase",
    "git revert",
    "git reset",
    "git rm",
    "git mv",
    "git clean",
    "git stash",
    "mv ",
    "cp ",
    "rm ",
    "mkdir ",
    "touch ",
    "chmod ",
    "chown ",
    "cargo fmt",
    "cargo fix",
    "cargo add",
    "cargo remove",
    "npm install",
    "npm ci",
    "pnpm install",
    "pip install",
];

const INVESTIGATE_COMMANDS: &[&str] = &[
    "grep",
    "rg",
    "find ",
    "fd",
    "ls",
    "cat ",
    "head ",
    "tail ",
    "wc ",
    "git log",
    "git show",
    "git blame",
    "git status",
    "git diff",
    "git grep",
    "which ",
    "file ",
    "stat ",
    "du ",
];

fn command_head_matches(segment: &str, head: &str) -> bool {
    let head = head.trim_end();
    segment == head || segment.starts_with(&format!("{head} "))
}

fn classify_command_segment(segment: &str) -> Option<Phase> {
    if VALIDATE_COMMANDS
        .iter()
        .any(|head| command_head_matches(segment, head))
    {
        return Some(Phase::Validate);
    }
    let modifies = MODIFY_COMMANDS
        .iter()
        .any(|head| command_head_matches(segment, head))
        || (segment.starts_with("sed ") && segment.split_whitespace().any(|f| f == "-i"));
    if modifies {
        return Some(Phase::Modify);
    }
    if INVESTIGATE_COMMANDS
        .iter()
        .any(|head| command_head_matches(segment, head))
    {
        return Some(Phase::Investigate);
    }
    None
}

fn classify_command(command: &str) -> Option<Phase> {
    let mut strongest: Option<Phase> = None;
    for part in command.split(['\n', ';', '|', '&']) {
        let segment = part.trim();
        if segment.is_empty() {
            continue;
        }
        match classify_command_segment(segment) {
            Some(Phase::Validate) => return Some(Phase::Validate),
            Some(Phase::Modify) => strongest = Some(Phase::Modify),
            Some(Phase::Investigate) if strongest.is_none() => strongest = Some(Phase::Investigate),
            _ => {}
        }
    }
    strongest
}

fn classify_tool(tool: &str) -> Option<Phase> {
    match tool {
        "read" | "grep" | "glob" | "ls" | "search" | "code_search" => Some(Phase::Investigate),
        "edit" | "write" | "multiedit" | "apply_patch" | "notebook_edit" => Some(Phase::Modify),
        _ => None,
    }
}

fn strongest_phase(current: Option<Phase>, candidate: Option<Phase>) -> Option<Phase> {
    fn rank(phase: Phase) -> u8 {
        match phase {
            Phase::Modify | Phase::Revise => 2,
            Phase::Validate => 1,
            Phase::Investigate => 0,
            _ => 0,
        }
    }
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(if rank(candidate) > rank(current) {
            candidate
        } else {
            current
        }),
        (Some(current), None) => Some(current),
        (None, candidate) => candidate,
    }
}

#[derive(Debug, Clone)]
struct ActivityBundle {
    event_indices: Vec<usize>,

    signal: Option<Phase>,
    phase: Phase,
    files: Vec<String>,
    commands: Vec<String>,
    outcomes: Vec<BashOutcome>,
}

impl ActivityBundle {
    fn has_failed_outcome(&self) -> bool {
        self.outcomes
            .iter()
            .any(|outcome| outcome.outcome == "failed")
    }
}

fn activity_bundles(events: &[EpisodeEvent]) -> Vec<ActivityBundle> {
    let mut bundles: Vec<ActivityBundle> = Vec::new();
    let mut index = 0;
    while index < events.len() {
        let head_group = events[index].group;
        let mut indices = vec![index];
        index += 1;
        while index < events.len() && events[index].group == head_group {
            indices.push(index);
            index += 1;
        }

        while let Some(event) = events.get(index) {
            let pairs = event.kind == EventKind::ToolResult
                && event.call_id.as_deref().is_some_and(|call_id| {
                    indices.iter().any(|i| {
                        events[*i].kind == EventKind::ToolCall
                            && events[*i].call_id.as_deref() == Some(call_id)
                    })
                });
            if !pairs {
                break;
            }
            indices.push(index);
            index += 1;
        }
        bundles.push(make_bundle(events, indices));
    }
    assign_bundle_phases(events, &mut bundles);
    bundles
}

fn make_bundle(events: &[EpisodeEvent], event_indices: Vec<usize>) -> ActivityBundle {
    let mut signal: Option<Phase> = None;
    let mut files: Vec<String> = Vec::new();
    let mut commands: Vec<String> = Vec::new();
    let mut outcomes: Vec<BashOutcome> = Vec::new();
    for index in &event_indices {
        let event = &events[*index];
        match event.kind {
            EventKind::UserText | EventKind::AssistantText => {}
            EventKind::ToolCall => {
                if let Some(command) = &event.command {
                    if !commands.contains(command) {
                        commands.push(command.clone());
                    }
                }
                for file in &event.files {
                    if !files.contains(file) {
                        files.push(file.clone());
                    }
                }
                let phase = if event.tool.as_deref() == Some("bash") {
                    event.command.as_deref().and_then(classify_command)
                } else {
                    event.tool.as_deref().and_then(classify_tool)
                };
                signal = strongest_phase(signal, phase);
            }
            EventKind::ToolResult => {
                if let Some(outcome) = &event.outcome {
                    outcomes.push(outcome.clone());
                }
            }
        }
    }
    ActivityBundle {
        event_indices,
        signal,
        phase: signal.unwrap_or(Phase::Orient),
        files,
        commands,
        outcomes,
    }
}

fn assign_bundle_phases(events: &[EpisodeEvent], bundles: &mut [ActivityBundle]) {
    let mut previous: Option<Phase> = None;
    for position in 0..bundles.len() {
        let contains_user = bundles[position]
            .event_indices
            .iter()
            .any(|index| events[*index].kind == EventKind::UserText);
        let is_last = position + 1 == bundles.len();
        let phase = if contains_user {
            Phase::Orient
        } else if let Some(signal) = bundles[position].signal {
            let revises = signal == Phase::Modify
                && position > 0
                && bundles[position - 1].phase == Phase::Validate
                && bundles[position - 1].has_failed_outcome();
            if revises {
                Phase::Revise
            } else {
                signal
            }
        } else if is_last {
            Phase::Resolve
        } else {
            previous.unwrap_or(Phase::Orient)
        };
        bundles[position].phase = phase;
        previous = Some(phase);
    }
}

fn bundle_body(events: &[EpisodeEvent], bundle: &ActivityBundle) -> String {
    let mut text = String::new();
    for index in &bundle.event_indices {
        let event = &events[*index];
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
            EventKind::ToolResult => {}
        }
    }
    for outcome in &bundle.outcomes {
        text.push_str("\nCommand: ");
        text.push_str(&outcome.command);
        text.push_str("\nOutcome: ");
        text.push_str(&outcome.outcome);
        text.push('\n');
    }
    text
}

#[derive(Debug, Clone)]
struct BoundaryCandidate {
    after_bundle: usize,
    score: i32,
    failed_validation: bool,
    file_set_change: bool,
    left_tokens: usize,
}

#[derive(Debug, Clone)]
struct SegmentDraft {
    start_bundle: usize,

    end_bundle: usize,
    boundary_reason: &'static str,
}

fn file_sets_disjoint(left: &[String], right: &[String]) -> bool {
    !left.is_empty() && !right.is_empty() && left.iter().all(|file| !right.contains(file))
}

fn boundary_score(
    bundles: &[ActivityBundle],
    boundary: usize,
    left_tokens: usize,
) -> (i32, bool, bool) {
    let left = &bundles[boundary - 1];
    let right = &bundles[boundary];
    let failed_validation = left.phase == Phase::Validate && left.has_failed_outcome();
    let work_cycle = matches!(right.phase, Phase::Modify | Phase::Revise)
        && matches!(left.phase, Phase::Validate | Phase::Investigate);
    let file_set_change = file_sets_disjoint(&left.files, &right.files);
    let mut score: i32 = 0;
    if failed_validation {
        score = 100;
    } else if work_cycle {
        score = 80;
    }
    if right.phase == Phase::Resolve {
        score = score.max(40);
    }
    if file_set_change {
        score = score.max(40);
    }
    if left_tokens.abs_diff(SEGMENT_TARGET_TOKENS) * 100 <= SEGMENT_TARGET_TOKENS * 15 {
        score += 10;
    }
    if left_tokens < SEGMENT_MIN_TOKENS {
        score -= 60;
    }
    (score, failed_validation, file_set_change)
}

fn select_segments(
    bundles: &[ActivityBundle],
    bundle_tokens: &[usize],
) -> (Vec<SegmentDraft>, Vec<BoundaryCandidate>) {
    let mut drafts: Vec<SegmentDraft> = Vec::new();
    let mut candidates: Vec<BoundaryCandidate> = Vec::new();
    if bundles.is_empty() {
        return (drafts, candidates);
    }
    let mut start = 0;
    let mut left_tokens = bundle_tokens[0];
    for boundary in 1..bundles.len() {
        let (score, failed, file_change) = boundary_score(bundles, boundary, left_tokens);
        candidates.push(BoundaryCandidate {
            after_bundle: boundary - 1,
            score,
            failed_validation: failed,
            file_set_change: file_change,
            left_tokens,
        });

        let right = &bundles[boundary];
        let overflow = left_tokens + bundle_tokens[boundary] > MAX_TOKENS;
        let work_cycle = !overflow
            && left_tokens >= SEGMENT_MIN_TOKENS
            && matches!(right.phase, Phase::Modify | Phase::Revise)
            && matches!(
                bundles[boundary - 1].phase,
                Phase::Validate | Phase::Investigate
            );
        let resolution =
            !overflow && right.phase == Phase::Resolve && left_tokens >= SEGMENT_TARGET_TOKENS;
        let file_split = !overflow && file_change && left_tokens >= SEGMENT_TARGET_TOKENS;

        let cut_at = if overflow {
            let winner = candidates
                .iter()
                .filter(|candidate| candidate.after_bundle >= start)
                .min_by(|a, b| {
                    b.score
                        .cmp(&a.score)
                        .then(
                            a.left_tokens
                                .abs_diff(SEGMENT_TARGET_TOKENS)
                                .cmp(&b.left_tokens.abs_diff(SEGMENT_TARGET_TOKENS)),
                        )
                        .then(a.after_bundle.cmp(&b.after_bundle))
                });
            winner.map(|candidate| candidate.after_bundle + 1)
        } else if work_cycle || resolution || file_split {
            Some(boundary)
        } else {
            None
        };

        if let Some(cut_at) = cut_at {
            let reason = if !overflow {
                if bundles[boundary - 1].phase == Phase::Validate
                    && bundles[boundary - 1].has_failed_outcome()
                {
                    "failed_validation_to_edit"
                } else if bundles[boundary - 1].phase == Phase::Validate {
                    "validation_complete"
                } else if bundles[boundary - 1].phase == Phase::Investigate {
                    "investigate_to_modify"
                } else if right.phase == Phase::Resolve {
                    "resolution"
                } else {
                    "file_set_change"
                }
            } else {
                overflow_reason(&candidates, bundles, start, cut_at)
            };
            drafts.push(SegmentDraft {
                start_bundle: start,
                end_bundle: cut_at,
                boundary_reason: reason,
            });
            start = cut_at;
            left_tokens = bundle_tokens[start..boundary].iter().sum();
        }
        left_tokens += bundle_tokens[boundary];
    }
    drafts.push(SegmentDraft {
        start_bundle: start,
        end_bundle: bundles.len(),
        boundary_reason: "episode_end",
    });

    while drafts.len() > 1 {
        let last = drafts.last().unwrap();
        let last_tokens: usize = bundle_tokens[last.start_bundle..last.end_bundle]
            .iter()
            .sum();
        if last_tokens >= SEGMENT_MIN_TOKENS {
            break;
        }
        let previous = &drafts[drafts.len() - 2];
        let previous_tokens: usize = bundle_tokens[previous.start_bundle..previous.end_bundle]
            .iter()
            .sum();
        if previous_tokens + last_tokens > MAX_TOKENS {
            break;
        }
        if previous.boundary_reason == "failed_validation_to_edit" {
            break;
        }
        let last = drafts.pop().unwrap();
        let previous = drafts.last_mut().unwrap();
        previous.end_bundle = last.end_bundle;
    }

    (drafts, candidates)
}

fn overflow_reason(
    candidates: &[BoundaryCandidate],
    bundles: &[ActivityBundle],
    start: usize,
    cut_at: usize,
) -> &'static str {
    let Some(winner) = candidates
        .iter()
        .filter(|candidate| candidate.after_bundle >= start)
        .min_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(a.after_bundle.cmp(&b.after_bundle))
        })
    else {
        return "token_limit";
    };
    if winner.failed_validation {
        "failed_validation_to_edit"
    } else if winner.score >= 80 {
        "work_cycle"
    } else if bundles[cut_at].phase == Phase::Resolve {
        "resolution"
    } else if winner.file_set_change {
        "file_set_change"
    } else {
        "token_limit"
    }
}

struct PlannedSegment {
    segment_id: String,
    phase: Phase,
    boundary_reason: &'static str,
    header: String,
    body: String,
    files: Vec<String>,
    commands: Vec<String>,
    outcomes: Vec<serde_json::Value>,
    start_byte: usize,
    end_byte: usize,
    is_tail: bool,
    prev_segment: Option<String>,
    next_segment: Option<String>,
}

fn segment_id_for(locator: &str, episode: &EpisodeTurn, segment_key: &str) -> String {
    hash_segments(&[
        SEGMENTATION_POLICY_VERSION,
        locator,
        episode.start_key(),
        segment_key,
    ])
}

fn planned_segments(
    episode: &EpisodeTurn,
    bundles: &[ActivityBundle],
    drafts: &[SegmentDraft],
    locator: &str,
    breadcrumb: &str,
) -> Vec<PlannedSegment> {
    let mut planned: Vec<PlannedSegment> = Vec::new();
    for (position, draft) in drafts.iter().enumerate() {
        let draft_bundles = &bundles[draft.start_bundle..draft.end_bundle];
        let mut event_indices: Vec<usize> = Vec::new();
        let mut body = String::new();
        let mut files: Vec<String> = Vec::new();
        let mut commands: Vec<String> = Vec::new();
        let mut outcomes: Vec<serde_json::Value> = Vec::new();
        for bundle in draft_bundles {
            body.push_str(&bundle_body(&episode.events, bundle));
            for index in &bundle.event_indices {
                event_indices.push(*index);
            }
            for file in &bundle.files {
                if !files.contains(file) {
                    files.push(file.clone());
                }
            }
            for command in &bundle.commands {
                if !commands.contains(command) {
                    commands.push(command.clone());
                }
            }
            for outcome in &bundle.outcomes {
                outcomes.push(outcome.to_json());
            }
        }
        let start_byte = event_indices
            .first()
            .map(|index| episode.events[*index].start_byte)
            .unwrap_or(0);
        let end_byte = event_indices
            .last()
            .map(|index| episode.events[*index].end_byte)
            .unwrap_or(0);
        let start_event_id = episode.events[event_indices[0]].id.clone();
        let phase = draft_bundles
            .last()
            .map(|bundle| bundle.phase)
            .unwrap_or(Phase::Orient);
        let header = format!("{breadcrumb} > segment {}", position + 1);
        let tokens = estimate_tokens(&format!("{header}\n\n{body}"));
        if tokens > MAX_TOKENS {
            let max_chars = (MAX_TOKENS * 4)
                .saturating_sub(header.chars().count() + 2)
                .max(1);
            for (piece, (piece_text, _, _)) in
                split_oversized(&body, max_chars).into_iter().enumerate()
            {
                let segment_key = if piece == 0 {
                    start_event_id.clone()
                } else {
                    format!("{start_event_id}#{piece}")
                };
                planned.push(PlannedSegment {
                    segment_id: segment_id_for(locator, episode, &segment_key),
                    phase,
                    boundary_reason: "message_split_last_resort",
                    header: header.clone(),
                    body: piece_text,
                    files: files.clone(),
                    commands: commands.clone(),
                    outcomes: outcomes.clone(),
                    start_byte,
                    end_byte,
                    is_tail: false,
                    prev_segment: None,
                    next_segment: None,
                });
            }
        } else {
            planned.push(PlannedSegment {
                segment_id: segment_id_for(locator, episode, &start_event_id),
                phase,
                boundary_reason: draft.boundary_reason,
                header,
                body,
                files,
                commands,
                outcomes,
                start_byte,
                end_byte,
                is_tail: false,
                prev_segment: None,
                next_segment: None,
            });
        }
    }
    let ids: Vec<String> = planned.iter().map(|s| s.segment_id.clone()).collect();
    let total = planned.len();
    for (position, segment) in planned.iter_mut().enumerate() {
        segment.is_tail = position + 1 == total;
        segment.prev_segment = if position > 0 {
            Some(ids[position - 1].clone())
        } else {
            None
        };
        segment.next_segment = ids.get(position + 1).cloned();
    }
    planned
}

fn segment_routing(episode: &EpisodeTurn, segment: &PlannedSegment, session_id: &str) -> String {
    let mut routing = format!(
        "source: agent_episode_segment\nsession: {session_id}\nepisode: {}\nphase: {}\nboundary: {}\n",
        episode.absolute_index + 1,
        segment.phase.as_str(),
        segment.boundary_reason
    );
    let user = episode.user_text();
    if !user.is_empty() {
        routing.push_str("user intent:\n");
        routing.push_str(&user.lines().take(4).collect::<Vec<_>>().join("\n"));
        routing.push('\n');
    }
    routing.push_str(&format!(
        "\nfiles read or edited:\n{}\n\ncommands:\n{}",
        segment.files.join("\n"),
        segment.commands.join("\n")
    ));
    routing
}

struct EpisodePlan {
    segment_records: Vec<serde_json::Value>,
    candidate_records: Vec<serde_json::Value>,
    event_records: Vec<serde_json::Value>,
    units: Vec<BuiltUnit>,
}

fn plan_episode(
    episode: &EpisodeTurn,
    session_id: &str,
    harness: HarnessLabel,
    breadcrumb: &str,
    atom_index: usize,
) -> EpisodePlan {
    let locator = harness.locator(session_id);
    let bundles = activity_bundles(&episode.events);
    let bundle_tokens: Vec<usize> = bundles
        .iter()
        .map(|bundle| estimate_tokens(&bundle_body(&episode.events, bundle)))
        .collect();
    let (drafts, candidates) = select_segments(&bundles, &bundle_tokens);
    let planned = planned_segments(episode, &bundles, &drafts, &locator, breadcrumb);

    let selected: BTreeSet<usize> = drafts
        .iter()
        .filter(|draft| draft.end_bundle < bundles.len())
        .map(|draft| draft.end_bundle)
        .collect();

    let segment_records = planned
        .iter()
        .map(|segment| {
            serde_json::json!({
                "segment_id": segment.segment_id,
                "phase": segment.phase.as_str(),
                "boundary_reason": segment.boundary_reason,
                "start_byte": segment.start_byte,
                "end_byte": segment.end_byte,
            })
        })
        .collect();
    let candidate_records = candidates
        .iter()
        .map(|candidate| {
            let left_phase = bundles[candidate.after_bundle].phase.as_str();
            let right_phase = bundles[candidate.after_bundle + 1].phase.as_str();
            serde_json::json!({
                "boundary_id": format!("boundary_{}", candidate.after_bundle + 1),
                "after_bundle": candidate.after_bundle,
                "features": {
                    "phase_transition": format!("{left_phase}->{right_phase}"),
                    "failed_validation": candidate.failed_validation,
                    "file_set_change": candidate.file_set_change,
                    "left_tokens": candidate.left_tokens,
                },
                "score": candidate.score,
                "selected": selected.contains(&(candidate.after_bundle + 1)),
            })
        })
        .collect();

    let mut units = Vec::new();
    for segment in &planned {
        let evidence = format!("{}\n\n{}", segment.header, segment.body);
        let routing = segment_routing(episode, segment, session_id);
        let token_count = estimate_tokens(&evidence);
        let oversized = token_count > MAX_TOKENS;
        let mut anchors = vec![BuiltAnchor {
            kind: AnchorKind::Session,
            value: session_id.to_string(),
            relationship: "part_of".to_string(),
            confidence: "deterministic".to_string(),
        }];
        for file in &segment.files {
            anchors.push(BuiltAnchor {
                kind: AnchorKind::File,
                value: file.clone(),
                relationship: "touched".to_string(),
                confidence: "deterministic".to_string(),
            });
        }
        let metadata = serde_json::json!({
            "session": session_id,
            "harness": harness.prefix,
            "episode": episode.absolute_index + 1,
            "segment_id": segment.segment_id,
            "phase": segment.phase.as_str(),
            "boundary_reason": segment.boundary_reason,
            "policy_version": SEGMENTATION_POLICY_VERSION,
            "is_tail": segment.is_tail,
            "prev_segment": segment.prev_segment,
            "next_segment": segment.next_segment,
            "source_range": {
                "start_byte": segment.start_byte,
                "end_byte": segment.end_byte,
            },
            "files": segment.files,
            "commands": segment.commands,
            "outcomes": segment.outcomes,
            "timestamp": episode.timestamp,
            "oversized": oversized,
            "indivisible_source_atom": oversized,
        });
        units.push(BuiltUnit {
            kind: UnitKind::EpisodeSegment,
            token_count,
            content_hash: hash_segments(&[
                SEGMENTATION_POLICY_VERSION,
                "episode_segment",
                &segment.segment_id,
                &evidence,
                &routing,
            ]),
            atom_indices: vec![atom_index],
            evidence_text: evidence,
            routing_text: routing,
            metadata,
            anchors,
        });
    }

    EpisodePlan {
        segment_records,
        candidate_records,
        event_records: episode.events.iter().map(EpisodeEvent::to_json).collect(),
        units,
    }
}

fn build_session(
    episodes: &[EpisodeTurn],
    session_id: &str,
    harness: HarnessLabel,
) -> (Vec<ParsedAtom>, Vec<BuiltUnit>) {
    let mut atoms = Vec::new();
    let mut units = Vec::new();
    for episode in episodes {
        let breadcrumb = format!(
            "{} > episode {}",
            harness.breadcrumb_root(session_id),
            episode.absolute_index + 1
        );
        let atom_index = atoms.len();
        let plan = plan_episode(episode, session_id, harness, &breadcrumb, atom_index);
        let text = episode.atom_text();
        let metadata = serde_json::json!({
            "session": session_id,
            "harness": harness.prefix,
            "episode": episode.absolute_index + 1,
            "timestamp": episode.timestamp,
            "files": episode.files(),
            "commands": episode.commands(),
            "outcomes": episode.outcomes(),
            "segment_policy": SEGMENTATION_POLICY_VERSION,
            "segments": plan.segment_records,
            "candidate_boundaries": plan.candidate_records,
            "events": plan.event_records,
        });
        let content_hash = ParsedAtom::content_hash_of(AtomKind::Episode, &breadcrumb, &text);
        atoms.push(ParsedAtom {
            kind: AtomKind::Episode,
            parent_index: None,
            ordinal: episode.absolute_index as u32,
            start_offset: 0,
            end_offset: text.len(),
            text,
            breadcrumb,
            content_hash,
            metadata,
        });
        units.extend(plan.units);
    }
    (atoms, units)
}

fn seal_episode(episodes: &mut Vec<EpisodeTurn>, current: &mut Option<EpisodeTurn>) {
    if let Some(episode) = current.take() {
        if !episode.events.is_empty() {
            episodes.push(episode);
        }
    }
}

fn finalize_episode_cap(episodes: &mut Vec<EpisodeTurn>) {
    if episodes.len() > MAX_EPISODES_PER_SESSION {
        let excess = episodes.len() - MAX_EPISODES_PER_SESSION;
        episodes.drain(..excess);
    }
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

fn parse_pi_episodes(content: &str) -> Vec<EpisodeTurn> {
    let mut episodes: Vec<EpisodeTurn> = Vec::new();
    let mut current: Option<EpisodeTurn> = None;
    let mut pending_bash: HashMap<String, String> = HashMap::new();
    let mut group: u32 = 0;
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
                seal_episode(&mut episodes, &mut current);
                group += 1;
                current = Some(EpisodeTurn {
                    role: "user".to_string(),
                    absolute_index: episodes.len(),
                    timestamp,
                    events: vec![EpisodeEvent {
                        id: event_id,
                        kind: EventKind::UserText,
                        group,
                        start_byte,
                        end_byte,
                        text,
                        tool: None,
                        call_id: None,
                        files: Vec::new(),
                        command: None,
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
                        role: "assistant".to_string(),
                        absolute_index: episodes.len(),
                        timestamp,
                        events: Vec::new(),
                    });
                }
                group += 1;
                let episode = current.as_mut().unwrap();
                if episode.role == "user" {
                    episode.role = "user+assistant".to_string();
                }
                if !text.is_empty() {
                    episode.events.push(EpisodeEvent {
                        id: event_id.clone(),
                        kind: EventKind::AssistantText,
                        group,
                        start_byte,
                        end_byte,
                        text,
                        tool: None,
                        call_id: None,
                        files: Vec::new(),
                        command: None,
                        outcome: None,
                    });
                }
                for (call_id, tool) in calls {
                    episode.events.push(EpisodeEvent {
                        id: call_id.clone(),
                        kind: EventKind::ToolCall,
                        group,
                        start_byte,
                        end_byte,
                        text: tool.summary,
                        tool: Some(tool.tool.to_ascii_lowercase()),
                        call_id: Some(call_id),
                        files: tool.files,
                        command: tool.command,
                        outcome: None,
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
                group += 1;
                if let Some(episode) = current.as_mut() {
                    episode.events.push(EpisodeEvent {
                        id: event_id,
                        kind: EventKind::ToolResult,
                        group,
                        start_byte,
                        end_byte,
                        text: String::new(),
                        tool: None,
                        call_id,
                        files: Vec::new(),
                        command: None,
                        outcome,
                    });
                }
            }
            _ => {}
        }
    }
    seal_episode(&mut episodes, &mut current);
    episodes
}

pub fn ingest_pi_session(
    content: &str,
    session_id: &str,
) -> Result<(Vec<ParsedAtom>, Vec<BuiltUnit>), Box<dyn std::error::Error + Send + Sync>> {
    let mut episodes = parse_pi_episodes(content);
    finalize_episode_cap(&mut episodes);
    Ok(build_session(&episodes, session_id, PI_HARNESS))
}

pub fn parse_session(
    content: &str,
    session_id: &str,
) -> Result<Vec<ParsedAtom>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(ingest_pi_session(content, session_id)?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"type":"session","version":3,"id":"s1","timestamp":"2026-08-26T18:00:00.000Z","cwd":"/tmp/repo"}
{"type":"model_change","id":"m1","parentId":null,"timestamp":"2026-08-26T18:00:00.100Z"}
{"type":"message","id":"u1","parentId":"m1","timestamp":"2026-08-26T18:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Investigate why refresh_session loops"}]}}
{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-08-26T18:01:05.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"src/auth.rs"}}]}}
{"type":"message","id":"r1","parentId":"a1","timestamp":"2026-08-26T18:01:05.100Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"read","content":[{"type":"text","text":"900 lines of file contents that must not become evidence"}]}}
{"type":"message","id":"a2","parentId":"r1","timestamp":"2026-08-26T18:02:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Found it. Stale tokens were reused."},{"type":"toolCall","id":"c2","name":"edit","arguments":{"path":"src/auth.rs","newText":"fn refresh() {}"}}]}}
{"type":"message","id":"u2","parentId":"a2","timestamp":"2026-08-26T18:05:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Now run the tests"}]}}
{"type":"message","id":"a3","parentId":"u2","timestamp":"2026-08-26T18:05:02.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"c3","name":"bash","arguments":{"command":"cargo test auth"}}]}}
{"type":"compaction","id":"k1","parentId":"a3","timestamp":"2026-08-26T18:06:00.000Z","summary":"old"}
{"type":"custom","id":"x1","parentId":"k1","timestamp":"2026-08-26T18:06:01.000Z","payload":{"anything":true}}
"#;

    #[test]
    fn episode_segmentation_splits_on_user_turns() {
        let atoms = parse_session(SAMPLE, "s1").unwrap();
        assert_eq!(atoms.len(), 2, "one episode per user turn");
        assert!(atoms[0]
            .text
            .contains("Investigate why refresh_session loops"));
        assert!(atoms[0]
            .text
            .contains("Found it. Stale tokens were reused."));
        assert!(atoms[1].text.contains("Now run the tests"));
        assert!(atoms[0].metadata["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "src/auth.rs"));
        assert!(atoms[1].metadata["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "cargo test auth"));
    }

    #[test]
    fn structured_bash_outcomes_are_captured_and_unknowns_stay_unknown() {
        let structured = format!(
            "{}\n{}\n{}\n{}",
            r#"{"type":"session","version":3,"id":"s2","cwd":"/tmp/repo"}"#,
            r#"{"type":"message","id":"a1","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"cargo test auth"}}]}}"#,
            r#"{"type":"message","id":"r1","message":{"role":"toolResult","content":[{"type":"toolResult","toolCallId":"c1","toolName":"bash","text":"{\"exitCode\":1,\"durationMs\":4200,\"failed\":2}"}]}}"#,
            r#"{"type":"message","id":"a2","message":{"role":"assistant","content":[{"type":"toolCall","id":"c2","name":"bash","arguments":{"command":"cargo build"}}]}}"#,
        );
        let unstructured_result = r#"{"type":"message","id":"r2","message":{"role":"toolResult","content":[{"type":"toolResult","toolCallId":"c2","toolName":"bash","text":"compile finished in one line of prose"}]}}"#;
        let (_, units) =
            ingest_pi_session(&format!("{structured}\n{unstructured_result}"), "s2").unwrap();
        assert_eq!(units.len(), 1, "one segment for the single-bundle episode");
        let outcomes = units[0].metadata["outcomes"].as_array().unwrap();
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0]["command"], "cargo test auth");
        assert_eq!(outcomes[0]["outcome"], "failed");
        assert_eq!(outcomes[0]["exit_code"], 1);
        assert_eq!(outcomes[0]["duration_ms"], 4200);
        assert_eq!(outcomes[0]["test_counts"]["failed"], 2);
        assert_eq!(outcomes[1]["outcome"], "unknown");
        assert!(outcomes[1].get("exit_code").is_none());
        assert!(units[0].evidence_text.contains("Command: cargo test auth"));
        assert!(units[0].evidence_text.contains("Outcome: failed"));
    }

    #[test]
    fn tool_outputs_never_become_evidence() {
        let atoms = parse_session(SAMPLE, "s1").unwrap();
        assert!(!atoms[0].text.contains("900 lines"));
        for atom in &atoms {
            assert!(!atom.text.contains("900 lines of file contents"));
        }
        let (_, units) = ingest_pi_session(SAMPLE, "s1").unwrap();
        for unit in &units {
            assert!(!unit.evidence_text.contains("900 lines of file contents"));
        }
    }

    #[test]
    fn segments_carry_policy_metadata_and_siblings() {
        let (atoms, units) = ingest_pi_session(SAMPLE, "s1").unwrap();
        assert_eq!(atoms.len(), 2);
        for unit in &units {
            assert_eq!(unit.kind, UnitKind::EpisodeSegment);
            assert_eq!(unit.metadata["policy_version"], SEGMENTATION_POLICY_VERSION);
            assert!(unit.metadata["segment_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()));
            assert!(unit.metadata["source_range"]["start_byte"].is_u64());
            assert!(unit.routing_text.contains("source: agent_episode_segment"));
            assert!(unit.routing_text.contains("phase: "));
        }
        for atom in &atoms {
            assert_eq!(atom.metadata["segment_policy"], SEGMENTATION_POLICY_VERSION);
            assert!(!atom.metadata["segments"].as_array().unwrap().is_empty());
            assert!(!atom.metadata["events"].as_array().unwrap().is_empty());
            assert!(atom.metadata["candidate_boundaries"].as_array().is_some());
        }

        let by_episode: BTreeSet<u64> = units
            .iter()
            .map(|unit| unit.metadata["episode"].as_u64().unwrap())
            .collect();
        for episode in by_episode {
            let siblings: Vec<&BuiltUnit> = units
                .iter()
                .filter(|unit| unit.metadata["episode"].as_u64() == Some(episode))
                .collect();
            let tails = siblings
                .iter()
                .filter(|unit| unit.metadata["is_tail"] == true)
                .count();
            assert_eq!(tails, 1);
            for (position, unit) in siblings.iter().enumerate() {
                let is_last = position + 1 == siblings.len();
                assert_eq!(unit.metadata["is_tail"].as_bool(), Some(is_last));
                if position > 0 {
                    let prev = siblings[position - 1].metadata["segment_id"]
                        .as_str()
                        .unwrap();
                    assert_eq!(
                        unit.metadata["prev_segment"].as_str(),
                        Some(prev),
                        "prev link must chain"
                    );
                }
                if !is_last {
                    let next = siblings[position + 1].metadata["segment_id"]
                        .as_str()
                        .unwrap();
                    assert_eq!(
                        unit.metadata["next_segment"].as_str(),
                        Some(next),
                        "next link must chain"
                    );
                }
            }
        }
    }

    #[test]
    fn timestamps_parse_from_iso8601() {
        assert_eq!(
            parse_timestamp("2026-08-26T18:01:00.000Z"),
            Some(1_787_767_260)
        );
        assert_eq!(parse_timestamp("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_timestamp("garbage"), None);
    }

    #[test]
    fn directory_name_matches_pi_convention() {
        assert_eq!(
            session_directory_name("/home/joshsimon/Projects/snoop"),
            "--home-joshsimon-Projects-snoop--"
        );
    }

    #[test]
    fn commands_classify_into_phases() {
        assert_eq!(classify_command("cargo test auth"), Some(Phase::Validate));
        assert_eq!(classify_command("cargo build"), Some(Phase::Validate));
        assert_eq!(classify_command("pytest -q"), Some(Phase::Validate));
        assert_eq!(classify_command("git status"), Some(Phase::Investigate));
        assert_eq!(classify_command("rg foo src/"), Some(Phase::Investigate));
        assert_eq!(
            classify_command("sed -i 's/a/b/' f.rs"),
            Some(Phase::Modify)
        );
        assert_eq!(
            classify_command("git add -A && cargo test"),
            Some(Phase::Validate)
        );
        assert_eq!(classify_command("git add src/auth.rs"), Some(Phase::Modify));
        assert_eq!(classify_command("echo hello"), None);
    }

    #[test]
    fn bundles_never_split_a_call_from_its_result() {
        let (_, units) = ingest_pi_session(SAMPLE, "s1").unwrap();

        let first_episode: Vec<&BuiltUnit> = units
            .iter()
            .filter(|unit| unit.metadata["episode"] == 1)
            .collect();
        assert!(!first_episode.is_empty());
        let atom = parse_session(SAMPLE, "s1").unwrap();
        let events = atom[0].metadata["events"].as_array().unwrap();
        let call = events
            .iter()
            .find(|event| event["call_id"] == "c1")
            .unwrap();
        let result = events
            .iter()
            .find(|event| event["kind"] == "tool_result" && event["call_id"] == "c1")
            .unwrap();
        let in_same_segment = first_episode.iter().any(|unit| {
            let range = &unit.metadata["source_range"];
            range["start_byte"].as_u64().unwrap() <= call["start_byte"].as_u64().unwrap()
                && range["end_byte"].as_u64().unwrap() >= result["end_byte"].as_u64().unwrap()
        });
        assert!(in_same_segment, "call and result must share a segment");
    }

    #[test]
    fn failed_validation_then_edit_cuts_a_segment() {
        let long = "Walk through the auth module carefully and explain every finding in detail. "
            .repeat(8);
        let text_only = format!(
            r#"{{"type":"message","id":"a1","message":{{"role":"assistant","content":[{{"type":"text","text":"{long}"}}]}}}}"#
        );
        let text_plus_edit = format!(
            r#"{{"type":"message","id":"a4","message":{{"role":"assistant","content":[{{"type":"text","text":"{long}"}} ,{{"type":"toolCall","id":"c3","name":"edit","arguments":{{"path":"src/auth.rs"}}}}]}}}}"#
        );
        let session = [
            r#"{"type":"session","version":3,"id":"s3","cwd":"/tmp/repo"}"#,
            r#"{"type":"message","id":"u1","message":{"role":"user","content":[{"type":"text","text":"Fix the failing refresh test"}]}}"#,
            text_only.as_str(),
            r#"{"type":"message","id":"a2","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"edit","arguments":{"path":"src/auth.rs"}}]}}"#,
            r#"{"type":"message","id":"c1r","message":{"role":"toolResult","content":[{"type":"toolResult","toolCallId":"c1","toolName":"edit","text":"ok"}]}}"#,
            r#"{"type":"message","id":"a3","message":{"role":"assistant","content":[{"type":"toolCall","id":"c2","name":"bash","arguments":{"command":"cargo test"}}]}}"#,
            r#"{"type":"message","id":"c2r","message":{"role":"toolResult","content":[{"type":"toolResult","toolCallId":"c2","toolName":"bash","text":"{\"exitCode\":1}"}]}}"#,
            text_plus_edit.as_str(),
            r#"{"type":"message","id":"a5","message":{"role":"assistant","content":[{"type":"toolCall","id":"c4","name":"bash","arguments":{"command":"cargo test"}}]}}"#,
        ]
            .join("\n");
        let (_, units) = ingest_pi_session(&session, "s3").unwrap();
        assert!(
            units.len() >= 2,
            "expected a cut after the failed validation"
        );
        let cut = units
            .iter()
            .find(|unit| unit.metadata["boundary_reason"] == "failed_validation_to_edit");
        assert!(
            cut.is_some(),
            "boundary reasons: {:?}",
            units
                .iter()
                .map(|unit| unit.metadata["boundary_reason"].clone())
                .collect::<Vec<_>>()
        );

        let failed_segment = cut.unwrap();
        assert!(failed_segment.evidence_text.contains("Outcome: failed"));
        assert!(failed_segment.evidence_text.contains("edit src/auth.rs"));
    }
}
