mod common;

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use common::{indexed_fixture, simple_fixture, HangingEmbedServer, McpChild};
use snoop::inference::MockEmbedder;
use snoop::ingest::index_repository_bounded;
use snoop::mcp::{handle_message, serve, ServeConfig, PROTOCOL_VERSION};
use snoop::store::Store;

fn memory_serve_config(embedder: Option<Arc<dyn snoop::inference::Embedder>>) -> ServeConfig {
    ServeConfig {
        open_store: Arc::new(|| Store::open_in_memory()),
        embedder,
        workers: 2,
        embed_deadline: Duration::from_secs(2),
    }
}

/// Runs `serve` to completion over an in-memory store and returns stdout.
fn serve_collect(config: ServeConfig, script: &str) -> String {
    let input = Cursor::new(script.as_bytes().to_vec());
    let mut output = Vec::new();
    serve(config, input, &mut output).unwrap();
    String::from_utf8(output).unwrap()
}

#[test]
fn protocol_lifecycle_initialize_list_call_and_errors() {
    let directory = tempfile::tempdir().unwrap();
    simple_fixture(directory.path());
    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let response = handle_message(
        &store,
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
        Some(&embedder),
        &serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    )
    .unwrap();
    assert!(ping["result"].as_object().unwrap().is_empty());

    let tools = handle_message(
        &store,
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
        ["get_repo_context", "repo_symbol_context"]
    );
    for tool in tools["result"]["tools"].as_array().unwrap() {
        assert!(!tool["description"].as_str().unwrap().is_empty());
        assert_eq!(tool["inputSchema"]["type"], "object");
    }

    let unknown = handle_message(
        &store,
        Some(&embedder),
        &serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}),
    )
    .unwrap();
    assert_eq!(unknown["error"]["code"], -32601);

    let misuse = handle_message(
        &store,
        Some(&embedder),
        &serde_json::json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "get_repo_context", "arguments": {}}}),
    )
    .unwrap();
    assert_eq!(misuse["error"]["code"], -32602);

    let empty_symbol = handle_message(
        &store,
        Some(&embedder),
        &serde_json::json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {"name": "repo_symbol_context", "arguments": {}}}),
    )
    .unwrap();
    assert_eq!(empty_symbol["result"]["isError"], true);

    let unknown_tool = handle_message(
        &store,
        Some(&embedder),
        &serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": "no_such_tool", "arguments": {}}}),
    )
    .unwrap();
    assert_eq!(unknown_tool["error"]["code"], -32602);

    let script = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
         not-json\n\
         {\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"ping\"}\n";
    let lines: Vec<serde_json::Value> = serve_collect(
        memory_serve_config(Some(Arc::new(MockEmbedder::new("mock-v1")))),
        script,
    )
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
        let status = std::process::Command::new("git")
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
        vec!["index", repo_arg, "--db", db.to_str().unwrap()],
    ] {
        let output = std::process::Command::new(binary)
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

    let mut child = std::process::Command::new(binary)
        .args(["mcp", "--db", db.to_str().unwrap()])
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
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
            "params":{"name":"repo_symbol_context","arguments":{"symbol":"refresh_session"}}}),
    ];
    {
        use std::io::Write;
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

    // Tool calls run on a worker pool, so responses may arrive out of order;
    // JSON-RPC allows this. Everything is keyed by id.
    let by_id = |id: i64| {
        responses
            .iter()
            .find(|response| response["id"] == id)
            .expect("every request is answered")
    };

    let initialize = by_id(1);
    assert_eq!(initialize["result"]["protocolVersion"], "2025-06-18");
    let names: Vec<&str> = by_id(2)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names.len(), 2);

    let get_context = by_id(3);
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
    let first_item = &packet["items"][0];
    for field in ["unit_id", "source_slices", "anchors", "selected_because"] {
        assert!(
            first_item.get(field).is_none(),
            "MCP packets stay lean: {field}"
        );
    }

    let symbol = by_id(4);
    let symbol_entries: serde_json::Value =
        serde_json::from_str(symbol["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let symbol_list = symbol_entries.as_array().unwrap();
    assert!(
        symbol_list
            .iter()
            .any(|entry| entry["source_kind"] == "code"),
        "repo_symbol_context answers current code"
    );
    let commits = symbol_list
        .iter()
        .filter(|entry| entry["source_kind"] == "git_commit")
        .collect::<Vec<_>>();
    assert!(!commits.is_empty(), "repo_symbol_context answers history");
    assert!(
        commits.iter().any(|entry| entry["evidence_text"]
            .as_str()
            .is_some_and(|text| text.contains("cache step"))),
        "commit entries carry evidence_text"
    );
    assert!(
        commits.iter().all(|entry| entry["timestamp"].is_i64()),
        "commit entries carry timestamp"
    );
}

#[test]
fn serve_exits_cleanly_at_eof() {
    let input = Cursor::new(Vec::new());
    let output = Vec::new();
    serve(
        memory_serve_config(Some(Arc::new(MockEmbedder::new("mock-v1")))),
        input,
        output,
    )
    .unwrap();
}

/// AP-1 invariant: the control plane never queues behind tool calls. Ping is
/// answered immediately even though three get_repo_context calls are stuck
/// inside a hung embedder.
#[test]
fn ping_answers_while_tool_calls_wait_on_a_hung_embedder() {
    let (_dir, db) = indexed_fixture();
    let embedder = HangingEmbedServer::spawn();
    let url = embedder.url();
    let mut child = McpChild::start(
        &db,
        &[
            ("SNOOP_EMBED_URL", &url),
            ("SNOOP_EMBED_DEADLINE_MS", "100"),
        ],
    );

    for id in 1..=3 {
        child.send(
            &serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":"get_repo_context","arguments":{"query":"auth rotation"}}}),
        );
    }
    std::thread::sleep(Duration::from_millis(20));
    let ping_sent = std::time::Instant::now();
    child.send(&serde_json::json!({"jsonrpc":"2.0","id":99,"method":"ping"}));

    // Workers may answer in any order; read all four responses. The
    // control-plane invariant is the ping's latency: the sequential loop
    // would have made ping wait K x hang (minutes), never milliseconds.
    let mut tool_ids = Vec::new();
    let mut ping_latency: Option<Duration> = None;
    for _ in 0..4 {
        let sent_for_this_response = std::time::Instant::now();
        let _ = sent_for_this_response;
        let response = child
            .read_response(Duration::from_secs(5))
            .expect("every request answered");
        if response["id"] == 99 {
            ping_latency = Some(ping_sent.elapsed());
        } else {
            assert_eq!(
                response["result"]["degraded"], true,
                "hung embedder degrades to lexical-only with a visible flag"
            );
            tool_ids.push(response["id"].as_i64().expect("tool response id"));
        }
    }
    let ping_latency = ping_latency.expect("ping answered");
    assert!(
        ping_latency < Duration::from_millis(500),
        "ping stayed fast under embed contention: {ping_latency:?}"
    );
    tool_ids.sort_unstable();
    assert_eq!(tool_ids, vec![1, 2, 3], "all tool calls answered");
}

/// AP-1 invariant: after the embed deadline fires three times in a row, the
/// breaker serves lexical-only immediately instead of paying the deadline.
#[test]
fn embed_breaker_opens_after_repeated_deadlines() {
    let (_dir, db) = indexed_fixture();
    let embedder = HangingEmbedServer::spawn();
    let url = embedder.url();
    let mut child = McpChild::start(
        &db,
        &[("SNOOP_EMBED_URL", &url), ("SNOOP_EMBED_DEADLINE_MS", "80")],
    );

    for id in 1..=3 {
        child.send(
            &serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":"get_repo_context","arguments":{"query":"auth rotation"}}}),
        );
        let sent = std::time::Instant::now();
        let response = child
            .read_response(Duration::from_secs(5))
            .expect("degraded answer");
        assert_eq!(response["result"]["degraded"], true);
        let elapsed = sent.elapsed();
        assert!(
            elapsed >= Duration::from_millis(80) && elapsed < Duration::from_millis(1000),
            "deadline bounds the wait: {elapsed:?}"
        );
    }

    // Breaker is open: the fourth query skips the embed entirely.
    child.send(
        &serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
        "params":{"name":"get_repo_context","arguments":{"query":"auth rotation"}}}),
    );
    let sent = std::time::Instant::now();
    let response = child
        .read_response(Duration::from_secs(5))
        .expect("breaker answer");
    assert_eq!(response["result"]["degraded"], true);
    let elapsed = sent.elapsed();
    assert!(
        elapsed < Duration::from_millis(50),
        "open breaker answers immediately: {elapsed:?}"
    );
}

/// Defect-audit c5: the store opens per job. A failed open fails only that
/// job (JSON-RPC error) and the worker keeps serving the next call.
#[test]
fn worker_survives_store_open_failure_and_serves_the_next_job() {
    let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let config = ServeConfig {
        open_store: {
            let opens = Arc::clone(&opens);
            Arc::new(move || {
                if opens.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    Store::open("/nonexistent-parent-dir/snoop-missing.db")
                } else {
                    Store::open_in_memory()
                }
            })
        },
        embedder: Some(Arc::new(MockEmbedder::new("mock-v1"))),
        workers: 1,
        embed_deadline: Duration::from_secs(2),
    };
    let script = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"repo_symbol_context\",\"arguments\":{\"symbol\":\"anything\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"repo_symbol_context\",\"arguments\":{\"symbol\":\"anything\"}}}\n",
    );
    let lines: Vec<serde_json::Value> = serve_collect(config, script)
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(lines.len(), 2, "both calls answered");
    let failed = lines.iter().find(|line| line["id"] == 1).unwrap();
    assert_eq!(failed["error"]["code"], -32603);
    assert!(
        failed["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("store open failed")),
        "the failure is the store open: {failed}"
    );
    let served = lines.iter().find(|line| line["id"] == 2).unwrap();
    assert!(
        served["result"].is_object(),
        "the worker must keep serving after a failed open: {served}"
    );
}

/// Defect-audit c5 regression: a worker that kept its spawn-time connection
/// answered from the unlinked inode after the database file was replaced by
/// a reindex. Answers must follow the database file.
#[test]
fn worker_answers_follow_the_database_file_across_reindex() {
    let (directory, db) = indexed_fixture();
    let mut child = McpChild::start(&db, &[("SNOOP_EMBED_URL", "mock")]);

    child.send(
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-06-18","capabilities":{}}}),
    );
    child.send(&serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    child.send(
        &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
        "params":{"name":"repo_symbol_context","arguments":{"symbol":"refresh_session"}}}),
    );
    let initialize = child
        .read_response(Duration::from_secs(5))
        .expect("initialize answered");
    assert_eq!(initialize["id"], 1);
    let before = child
        .read_response(Duration::from_secs(5))
        .expect("first call answered");
    assert_eq!(before["id"], 2);
    assert!(before["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("refresh_session"));

    // Replace the database file underneath the running server: remove it
    // with its WAL sidecars, then reindex with a new symbol into a fresh
    // file at the same path.
    for suffix in ["", "-wal", "-shm"] {
        let path = std::path::PathBuf::from(format!("{}{}", db.display(), suffix));
        let _ = std::fs::remove_file(&path);
    }
    let repo = directory.path().join("repo");
    std::fs::write(
        repo.join("src/extra.rs"),
        "pub fn brand_new_symbol() {\n    helper();\n}\n\nfn helper() {}\n",
    )
    .unwrap();
    let repo_arg = repo.to_str().unwrap();
    let db_arg = db.to_str().unwrap();
    for args in [
        vec!["init", repo_arg, "--db", db_arg],
        vec!["index", repo_arg, "--db", db_arg],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_snoop"))
            .args(&args)
            .env("SNOOP_EMBED_URL", "mock")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    child.send(
        &serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"repo_symbol_context","arguments":{"symbol":"brand_new_symbol"}}}),
    );
    let after = child
        .read_response(Duration::from_secs(5))
        .expect("post-reindex call answered");
    assert_eq!(after["id"], 3);
    let entries: serde_json::Value =
        serde_json::from_str(after["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let list = entries.as_array().expect("array payload");
    assert!(
        list.iter().any(|entry| entry["routing_text"]
            .as_str()
            .is_some_and(|text| text.contains("brand_new_symbol"))),
        "the worker must answer from the replaced database file, not the stale inode: {list:?}"
    );
}
