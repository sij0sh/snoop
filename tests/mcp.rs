use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use snoop::core::RepoId;
use snoop::inference::MockEmbedder;
use snoop::ingest::index_repository_bounded;
use snoop::mcp::{handle_message, serve, PROTOCOL_VERSION};
use snoop::store::Store;

fn simple_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session() {\n    validate();\n    rotate();\n}\n\nfn validate() {}\n\nfn rotate() {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("README.md"),
        "# Auth\n\n`refresh_session` rotates the session token after validation.\n",
    )
    .unwrap();
}

#[test]
fn protocol_lifecycle_initialize_list_call_and_errors() {
    let directory = tempfile::tempdir().unwrap();
    simple_fixture(directory.path());
    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let response = handle_message(
        &store,
        outcome.repo_id,
        Some(&embedder),
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}}
        }),
    )
    .unwrap();
    assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(response["result"]["serverInfo"]["name"], "snoop");
    assert!(response["result"]["capabilities"]["tools"].is_object());

    let ping = handle_message(
        &store,
        outcome.repo_id,
        Some(&embedder),
        &serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    )
    .unwrap();
    assert!(ping["result"].as_object().unwrap().is_empty());

    let tools = handle_message(
        &store,
        outcome.repo_id,
        Some(&embedder),
        &serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
    )
    .unwrap();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        names,
        ["get_repo_context", "repo_symbol_context", "repo_history"]
    );
    for tool in tools["result"]["tools"].as_array().unwrap() {
        assert!(!tool["description"].as_str().unwrap().is_empty());
        assert_eq!(tool["inputSchema"]["type"], "object");
    }

    let unknown = handle_message(
        &store,
        outcome.repo_id,
        Some(&embedder),
        &serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}),
    )
    .unwrap();
    assert_eq!(unknown["error"]["code"], -32601);

    let misuse = handle_message(
        &store,
        outcome.repo_id,
        Some(&embedder),
        &serde_json::json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "get_repo_context", "arguments": {}}}),
    )
    .unwrap();
    assert_eq!(misuse["error"]["code"], -32602);

    let empty_symbol = handle_message(
        &store,
        outcome.repo_id,
        Some(&embedder),
        &serde_json::json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {"name": "repo_history", "arguments": {}}}),
    )
    .unwrap();
    assert_eq!(empty_symbol["result"]["isError"], true);

    let unknown_tool = handle_message(
        &store,
        outcome.repo_id,
        Some(&embedder),
        &serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": "no_such_tool", "arguments": {}}}),
    )
    .unwrap();
    assert_eq!(unknown_tool["error"]["code"], -32602);

    let script = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
         not-json\n\
         {\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"ping\"}\n"
        .to_string();
    let mut input = script.as_bytes();
    let mut output = Vec::new();
    serve(
        &store,
        outcome.repo_id,
        Some(&embedder),
        &mut input,
        &mut output,
    )
    .unwrap();
    let lines: Vec<serde_json::Value> = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(lines.len(), 2, "notification silent, other two answered");
    assert_eq!(lines[0]["error"]["code"], -32700);
    assert_eq!(lines[1]["id"], 8);
}

#[test]
fn external_client_answers_fixture_questions_through_mcp_alone() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    let sessions_root = tempfile::tempdir().unwrap();

    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::write(
        repo.join("src/auth.rs"),
        "pub fn refresh_session() {\n    validate();\n    rotate();\n}\n\nfn validate() {}\n\nfn rotate() {}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("docs/auth-plan.md"),
        "# Rotation\n\n`refresh_session` validates the token before rotating.\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
            .env("GIT_AUTHOR_DATE", "2026-08-20T12:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-08-20T12:00:00Z")
            .status()
            .expect("git spawns");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "--quiet"]);
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "introduce refresh_session"]);
    std::fs::write(
        repo.join("src/auth.rs"),
        "pub fn refresh_session() {\n    validate();\n    rotate();\n    cache();\n}\n\nfn validate() {}\n\nfn rotate() {}\n\nfn cache() {}\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&[
        "commit",
        "--quiet",
        "-m",
        "add cache step to refresh_session",
    ]);

    let canonical = repo.canonicalize().unwrap();
    let session_dir = sessions_root
        .path()
        .join(snoop::ingest::harness::session_directory_name(
            &canonical.to_string_lossy(),
        ));
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("2026-08-21T09-00-00-000Z_mcp-session.jsonl"),
        concat!(
            r#"{"type":"session","version":3,"id":"mcp-session","timestamp":"2026-08-21T09:00:00.000Z","cwd":"/tmp/fixture"}"#, "\n",
            r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2026-08-21T09:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Investigate the refresh_session rotation order"}]}}"#, "\n",
            r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-08-21T09:01:30.000Z","message":{"role":"assistant","content":[{"type":"text","text":"The rotation ordering was deliberate to prevent stale reuse."}]}}"#, "\n",
        ),
    )
    .unwrap();

    let db = directory.path().join("index.db");
    let binary = env!("CARGO_BIN_EXE_snoop");
    let repo_arg = repo.to_str().unwrap();
    let sessions_arg = sessions_root.path().to_str().unwrap();
    let env = [
        ("SNOOP_EMBED_URL", "mock"),
        ("SNOOP_SESSIONS_ROOT", sessions_arg),
    ];

    for args in [
        vec!["init", repo_arg, "--db", db.to_str().unwrap()],
        vec!["index", "--db", db.to_str().unwrap(), "--repo", repo_arg],
    ] {
        let output = Command::new(binary)
            .args(&args)
            .envs(env.iter().copied())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut child = Command::new(binary)
        .args(["mcp", "--db", db.to_str().unwrap(), "--repo", repo_arg])
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let requests = [
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-06-18","capabilities":{}}}),
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"get_repo_context","arguments":{
                "query":"why does refresh_session rotate the token","max_tokens":2000}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"repo_history","arguments":{"symbol":"refresh_session"}}}),
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"repo_symbol_context","arguments":{"symbol":"refresh_session"}}}),
    ];
    {
        let stdin = child.stdin.as_mut().unwrap();
        for request in &requests {
            serde_json::to_writer(&mut *stdin, request).unwrap();
            stdin.write_all(b"\n").unwrap();
        }
        stdin.flush().unwrap();
    }
    drop(child.stdin.take());
    let stdout = child.wait_with_output().unwrap().stdout;
    let responses: Vec<serde_json::Value> = String::from_utf8_lossy(&stdout)
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        responses.len(),
        requests.len() - 1,
        "notification got no response"
    );

    let initialize = &responses[0];
    assert_eq!(initialize["result"]["protocolVersion"], "2025-06-18");
    let names: Vec<&str> = responses[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names.len(), 3);

    let get_context = &responses[2];
    assert!(get_context["result"]["isError"].is_null());
    let packet: serde_json::Value = serde_json::from_str(
        get_context["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let kinds: Vec<&str> = packet["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["source_kind"].as_str())
        .collect();
    assert!(kinds.contains(&"Code"), "code evidence via MCP: {kinds:?}");
    assert!(
        kinds.contains(&"GitCommit"),
        "history evidence via MCP: {kinds:?}"
    );
    assert!(
        kinds.contains(&"AgentSession"),
        "prior agent work via MCP: {kinds:?}"
    );

    let history = &responses[3];
    let history_entries: serde_json::Value =
        serde_json::from_str(history["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let history_list = history_entries.as_array().unwrap();
    assert!(!history_list.is_empty(), "repo_history answers commits");
    assert!(history_list.iter().any(|entry| entry["evidence_text"]
        .as_str()
        .is_some_and(|text| text.contains("cache step"))));

    let symbol = &responses[4];
    let symbol_entries: serde_json::Value =
        serde_json::from_str(symbol["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let symbol_list = symbol_entries.as_array().unwrap();
    assert!(
        symbol_list
            .iter()
            .any(|entry| entry["source_kind"] == "code"),
        "repo_symbol_context answers current code"
    );
    assert!(
        symbol_list
            .iter()
            .any(|entry| entry["source_kind"] == "git_commit"),
        "repo_symbol_context answers the symbol's history"
    );
}

#[test]
fn serve_exits_cleanly_at_eof() {
    let store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let mut input = b"".as_slice();
    let mut output = Vec::new();
    serve(&store, RepoId(1), Some(&embedder), &mut input, &mut output).unwrap();
    assert!(output.is_empty());
}
