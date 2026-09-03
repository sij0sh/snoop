//! Tool-call argument reduction: paths, commands, and one-line summaries.
//!
//! Shared by the Pi and Muse session families. Both adapters turn committed
//! tool calls into compact evidence summaries plus structured outcomes; raw
//! tool-result bodies never enter the index.

/// Structured outcome attached to one committed tool call. Raw result text
/// is dropped after these facts are extracted.
#[derive(Debug, Clone)]
pub(crate) struct ToolOutcome {
    pub(crate) command: String,
    pub(crate) outcome: String,
    pub(crate) exit_code: Option<i64>,
    pub(crate) duration_ms: Option<i64>,
    pub(crate) test_counts: Option<serde_json::Value>,
}

impl ToolOutcome {
    pub(crate) fn unknown(command: String) -> Self {
        Self {
            command: command.chars().take(200).collect(),
            outcome: "unknown".to_string(),
            exit_code: None,
            duration_ms: None,
            test_counts: None,
        }
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
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
pub(crate) struct ToolRef {
    pub(crate) summary: String,
    pub(crate) files: Vec<String>,
    pub(crate) command: Option<String>,
}

pub(crate) fn extract_file(value: &str) -> Option<String> {
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

/// Upgrades a call outcome from one decoded result object. Accepts both the
/// Pi camelCase keys (`exitCode`, `durationMs`) and the Muse snake_case
/// keys (`exit_code`, `duration_ms`); a value carrying neither leaves the
/// outcome untouched.
pub(crate) fn upgrade_outcome_from_value(value: &serde_json::Value, outcome: &mut ToolOutcome) {
    let exit = value
        .get("exitCode")
        .and_then(|value| value.as_i64())
        .or_else(|| value.get("exit_code").and_then(|value| value.as_i64()));
    let duration = value
        .get("durationMs")
        .and_then(|value| value.as_i64())
        .or_else(|| value.get("duration_ms").and_then(|value| value.as_i64()));
    let counts = ["testsPassed", "passed", "failed", "total"]
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

/// Upgrades a call outcome from raw result text when that text is a JSON
/// object carrying exit/duration/count facts. Anything else (file contents,
/// listings, prose) leaves the outcome untouched.
pub(crate) fn upgrade_outcome_from_text(raw: &str, outcome: &mut ToolOutcome) {
    let trimmed = raw.trim();
    if !trimmed.starts_with('{') {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return;
    };
    upgrade_outcome_from_value(&value, outcome);
}

pub(crate) fn tool_ref(name: &str, arguments: &serde_json::Value) -> ToolRef {
    let mut files = Vec::new();
    let mut command = None;
    let mut summary = String::new();
    // Muse records file tools as read_file/edit_file/write_file; Pi uses
    // the short forms. Both spellings reduce to the same path extraction.
    let lowered = name.to_ascii_lowercase();
    let canonical = lowered.strip_suffix("_file").unwrap_or(lowered.as_str());
    match canonical {
        "read" | "edit" | "write" => {
            if let Some(path) = arguments["path"].as_str() {
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
        summary,
        files,
        command,
    }
}
