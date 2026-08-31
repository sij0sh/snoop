use super::*;
use std::time::Duration;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn json_merge_preserves_existing_content_and_sets_entry() {
    let dir = tempdir();
    let path = dir.path().join("claude.json");
    std::fs::write(
        &path,
        r#"{"mcpServers":{"other":{"command":"x"}},"model":"sonnet"}"#,
    )
    .unwrap();
    let outcome = merge::merge_json_entry(&path, "mcpServers", "snoop", &snoop_command_entry())
        .expect("merge succeeds");
    assert_eq!(outcome, WireOutcome::Wired);
    let root: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(root["mcpServers"]["other"]["command"], "x");
    assert_eq!(root["model"], "sonnet");
    assert_eq!(root["mcpServers"]["snoop"]["command"], "snoop");
    assert_eq!(root["mcpServers"]["snoop"]["args"][0], "mcp");
}

#[test]
fn json_merge_second_run_is_already_configured_noop() {
    let dir = tempdir();
    let path = dir.path().join("claude.json");
    std::fs::write(&path, r#"{"model":"sonnet"}"#).unwrap();
    merge::merge_json_entry(&path, "mcpServers", "snoop", &snoop_command_entry())
        .expect("first merge");
    let before = std::fs::read(&path).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let outcome = merge::merge_json_entry(&path, "mcpServers", "snoop", &snoop_command_entry())
        .expect("second merge");
    assert_eq!(outcome, WireOutcome::AlreadyConfigured);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
    let leftover: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(leftover.len(), 1, "no temp files left behind");
}

#[test]
fn json_merge_reports_updated_when_entry_differs() {
    let dir = tempdir();
    let path = dir.path().join("cursor.json");
    std::fs::write(
        &path,
        r#"{"mcpServers":{"snoop":{"command":"old"},"keep":{}}}"#,
    )
    .unwrap();
    let outcome = merge::merge_json_entry(&path, "mcpServers", "snoop", &snoop_command_entry())
        .expect("merge succeeds");
    assert_eq!(outcome, WireOutcome::Updated);
    let root: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(root["mcpServers"]["snoop"]["command"], "snoop");
    assert_eq!(root["mcpServers"]["keep"], serde_json::json!({}));
}

#[test]
fn json_merge_creates_missing_file() {
    let dir = tempdir();
    let path = dir.path().join("gemini").join("settings.json");
    let outcome = merge::merge_json_entry(&path, "mcpServers", "snoop", &snoop_command_entry())
        .expect("merge succeeds");
    assert_eq!(outcome, WireOutcome::Wired);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with('\n'), "trailing newline written");
    let root: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(root["mcpServers"]["snoop"]["command"], "snoop");
}

#[test]
fn jsonc_parse_failure_returns_snippet_error_and_preserves_file() {
    let dir = tempdir();
    let path = dir.path().join("claude.json");
    let original = "{\n  // user comment\n  \"model\": \"sonnet\",\n}";
    std::fs::write(&path, original).unwrap();
    let error = merge::merge_json_entry(&path, "mcpServers", "snoop", &snoop_command_entry())
        .expect_err("JSONC comments are unparseable");
    assert!(
        error.contains(&path.display().to_string()),
        "error must contain the config path: {error}"
    );
    assert!(
        error.contains("\"snoop\""),
        "error must contain the entry: {error}"
    );
    assert!(
        error.contains("mcpServers"),
        "error must contain the root key: {error}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn toml_merge_preserves_existing_comment_and_tables() {
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "# keep me\nmodel = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"x\"\n",
    )
    .unwrap();
    let outcome = merge::merge_toml_entry(&path, "mcp_servers", "snoop").expect("merge succeeds");
    assert_eq!(outcome, WireOutcome::Wired);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# keep me"), "comment preserved: {text}");
    assert!(text.contains("model = \"gpt-5\""));
    assert!(text.contains("[mcp_servers.other]"));
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(
        doc["mcp_servers"]["snoop"]["command"].as_str(),
        Some("snoop")
    );
}

#[test]
fn toml_merge_is_idempotent_and_reports_updates() {
    let dir = tempdir();
    let path = dir.path().join("config.toml");
    assert_eq!(
        merge::merge_toml_entry(&path, "mcp_servers", "snoop").unwrap(),
        WireOutcome::Wired
    );
    assert_eq!(
        merge::merge_toml_entry(&path, "mcp_servers", "snoop").unwrap(),
        WireOutcome::AlreadyConfigured
    );
    std::fs::write(&path, "[mcp_servers.snoop]\ncommand = \"old\"\n").unwrap();
    assert_eq!(
        merge::merge_toml_entry(&path, "mcp_servers", "snoop").unwrap(),
        WireOutcome::Updated
    );
    let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(
        doc["mcp_servers"]["snoop"]["command"].as_str(),
        Some("snoop")
    );
    assert_eq!(doc["mcp_servers"]["snoop"]["args"][0].as_str(), Some("mcp"));
}

#[test]
fn agent_name_validation_rejects_unknown_with_valid_list() {
    let valid = validate_agent_names(&["pi".to_string(), "codex".to_string()]).unwrap();
    assert_eq!(valid, vec!["pi", "codex"]);
    let error = validate_agent_names(&["nope".to_string()]).unwrap_err();
    assert!(error.contains("nope"), "error names the bad agent: {error}");
    for name in AGENT_NAMES {
        assert!(
            error.contains(name),
            "error lists valid name {name}: {error}"
        );
    }
}

#[test]
fn pi_wiring_copies_embedded_extension() {
    let dir = tempdir();
    let dest = dir.path().join("extensions").join("snoop-pi.ts");
    assert_eq!(wire_pi_to(&dest).unwrap(), WireOutcome::Wired);
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        PI_EXTENSION_TS.as_bytes(),
        "installed bytes match the embedded extension"
    );
    assert_eq!(wire_pi_to(&dest).unwrap(), WireOutcome::AlreadyConfigured);
    std::fs::write(&dest, "outdated").unwrap();
    assert_eq!(wire_pi_to(&dest).unwrap(), WireOutcome::Updated);
    assert_eq!(std::fs::read(&dest).unwrap(), PI_EXTENSION_TS.as_bytes());
}

#[test]
fn install_rejects_unknown_target_and_mixed_embedder_flags() {
    let error = run_install(InstallOptions {
        target: Some("nonsense".to_string()),
        list: false,
        agents: vec![],
        dir: None,
        force: false,
        model_url: None,
        version: None,
    })
    .unwrap_err();
    assert!(error.contains("unknown install target"), "{error}");
    let error = run_install(InstallOptions {
        target: Some("embedder".to_string()),
        list: false,
        agents: vec!["pi".to_string()],
        dir: None,
        force: false,
        model_url: None,
        version: None,
    })
    .unwrap_err();
    assert!(error.contains("--agent"), "{error}");
}

#[test]
fn opencode_entry_shape() {
    assert_eq!(
        opencode_entry(),
        serde_json::json!({
            "type": "local",
            "command": ["snoop", "mcp"],
            "enabled": true,
        })
    );
}
