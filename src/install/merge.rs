//! Config merging for agent wiring: JSON and TOML entry insertion with
//! atomic writes and no-clobber failure modes.

use std::fs;
use std::path::Path;

use super::WireOutcome;

pub fn merge_json_entry(
    path: &Path,
    root_key: &str,
    agent_key: &str,
    entry: &serde_json::Value,
) -> Result<WireOutcome, String> {
    let mut root = match read_json_or_empty(path) {
        Ok(root) => root,
        Err(reason) => return Err(unparseable_error(path, root_key, agent_key, entry, reason)),
    };
    if !root.is_object() {
        return Err(unparseable_error(
            path,
            root_key,
            agent_key,
            entry,
            "root is not a JSON object".to_string(),
        ));
    }
    let existing = get_nested(&root, &[root_key, agent_key]);
    if existing == Some(entry) {
        return Ok(WireOutcome::AlreadyConfigured);
    }
    let updated = existing.is_some();
    set_nested(&mut root, &[root_key, agent_key], entry.clone());
    write_json_atomic(path, &root)?;
    Ok(if updated {
        WireOutcome::Updated
    } else {
        WireOutcome::Wired
    })
}

fn read_json_or_empty(path: &Path) -> Result<serde_json::Value, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({}));
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    serde_json::from_str(&text).map_err(|error| format!("parse failed ({error})"))
}

fn unparseable_error(
    path: &Path,
    root_key: &str,
    agent_key: &str,
    entry: &serde_json::Value,
    reason: String,
) -> String {
    let snippet = serde_json::json!({ root_key: { agent_key: entry.clone() } }).to_string();
    format!(
        "cannot update {} ({reason}); add manually: {snippet}",
        path.display()
    )
}

fn get_nested<'a>(root: &'a serde_json::Value, keys: &[&str]) -> Option<&'a serde_json::Value> {
    let mut current = root;
    for key in keys {
        current = current.get(key)?;
    }
    Some(current)
}

fn set_nested(root: &mut serde_json::Value, keys: &[&str], value: serde_json::Value) {
    let mut current = root;
    for key in keys {
        if !current.is_object() {
            *current = serde_json::Value::Object(serde_json::Map::new());
        }
        let map = current.as_object_mut().expect("checked object");
        current = map
            .entry(key.to_string())
            .or_insert(serde_json::Value::Null);
    }
    *current = value;
}

fn write_json_atomic(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    text.push('\n');
    write_atomic(path, text.as_bytes())
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("no file name in {}", path.display()))?;
    let tmp = path.with_file_name(format!(".{}.snoop-tmp", file_name.to_string_lossy()));
    fs::write(&tmp, bytes).map_err(|error| format!("write {}: {error}", tmp.display()))?;
    fs::rename(&tmp, path)
        .map_err(|error| format!("rename {} -> {}: {error}", tmp.display(), path.display()))?;
    Ok(())
}

pub fn merge_toml_entry(
    path: &Path,
    root_key: &str,
    agent_key: &str,
) -> Result<WireOutcome, String> {
    let mut doc = match fs::read_to_string(path) {
        Ok(text) => text.parse::<toml_edit::DocumentMut>().map_err(|error| {
            format!(
                "cannot update {} (invalid TOML: {error}); add manually:\n\
                 [{root_key}.{agent_key}]\n\
                 command = \"snoop\"\n\
                 args = [\"mcp\"]",
                path.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let existing = toml_get_table(doc.as_table(), &[root_key, agent_key]);
    let already = existing
        .and_then(|table| table.get("command"))
        .and_then(|item| item.as_str())
        == Some("snoop")
        && existing
            .and_then(|table| table.get("args"))
            .map(toml_args_match)
            .unwrap_or(false);
    if already {
        return Ok(WireOutcome::AlreadyConfigured);
    }
    let updated = existing.is_some();
    let servers = doc
        .as_table_mut()
        .entry(root_key)
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| {
            format!(
                "cannot update {}: [{root_key}] is not a table",
                path.display()
            )
        })?;
    let snoop = servers
        .entry(agent_key)
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| {
            format!(
                "cannot update {}: [{root_key}.{agent_key}] is not a table",
                path.display()
            )
        })?;
    snoop["command"] = toml_edit::value("snoop");
    snoop["args"] = toml_edit::value(toml_edit::Array::from_iter(["mcp"]));
    write_atomic(path, doc.to_string().as_bytes())?;
    Ok(if updated {
        WireOutcome::Updated
    } else {
        WireOutcome::Wired
    })
}

fn toml_get_table<'a>(table: &'a toml_edit::Table, path: &[&str]) -> Option<&'a toml_edit::Table> {
    let mut current = table;
    for key in path {
        current = current.get(key)?.as_table()?;
    }
    Some(current)
}

fn toml_args_match(item: &toml_edit::Item) -> bool {
    match item.as_array() {
        Some(array) => {
            array.len() == 1
                && array
                    .iter()
                    .zip(["mcp"])
                    .all(|(value, expected)| value.as_str() == Some(expected))
        }
        None => false,
    }
}
