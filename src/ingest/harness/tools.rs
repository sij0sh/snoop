//! Tool-call argument reduction: paths, commands, and one-line summaries.

#[derive(Debug, Clone)]
pub(super) struct ToolRef {
    pub(super) summary: String,
    pub(super) files: Vec<String>,
    pub(super) command: Option<String>,
}

pub(super) fn extract_file(value: &str) -> Option<String> {
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

pub(super) fn tool_ref(name: &str, arguments: &serde_json::Value) -> ToolRef {
    let mut files = Vec::new();
    let mut command = None;
    let mut summary = String::new();
    let lowered = name.to_ascii_lowercase();
    match lowered.as_str() {
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
