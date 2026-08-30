use std::io::{BufRead, Write};

use crate::core::{RepoId, SourceKind};
use crate::inference::Embedder;
use crate::runtime::{query, QueryChannels, QueryOptions};
use crate::store::Store;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const DEFAULT_MAX_TOKENS: usize = 6_000;
const MAX_TOKENS_LIMIT: usize = 32_000;
const ANCHOR_LOOKUP_LIMIT: usize = 64;

type Error = Box<dyn std::error::Error + Send + Sync>;

pub fn symbol_context_entries(
    store: &Store,
    repo_id: RepoId,
    symbol: &str,
) -> Result<Vec<serde_json::Value>, Error> {
    let mut report = Vec::new();
    for id in store.units_for_anchor(repo_id, "symbol", symbol, ANCHOR_LOOKUP_LIMIT)? {
        if let Some(unit) = store.unit_by_id(id)? {
            report.push(serde_json::json!({
                "unit_id": id,
                "source_kind": unit.source_kind.as_str(),
                "locator": unit.locator,
                "routing_text": unit.routing_text,
            }));
        }
    }
    Ok(report)
}

pub fn history_entries(
    store: &Store,
    repo_id: RepoId,
    symbol: &str,
) -> Result<Vec<serde_json::Value>, Error> {
    let mut history = Vec::new();
    for id in store.units_for_anchor(repo_id, "symbol", symbol, ANCHOR_LOOKUP_LIMIT)? {
        if let Some(unit) = store.unit_by_id(id)? {
            if unit.source_kind == SourceKind::GitCommit {
                history.push(serde_json::json!({
                    "unit_id": id,
                    "locator": unit.locator,
                    "timestamp": unit.timestamp,
                    "evidence_text": unit.evidence_text,
                }));
            }
        }
    }
    Ok(history)
}

fn tool_definitions() -> serde_json::Value {
    serde_json::json!([
        {
            "name": "get_repo_context",
            "description": "Return a token-budgeted context packet of repository evidence \
                            (current code, docs, git history, prior agent work) for a query.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Natural-language question"},
                    "max_tokens": {
                        "type": "integer",
                        "description": "Evidence token budget: the sum of admitted evidence never exceeds it (default 6000)",
                        "default": DEFAULT_MAX_TOKENS
                    }
                },
                "required": ["query"]
            }
        },
        {
            "name": "repo_symbol_context",
            "description": "Return all units (code, docs, commits, agent episodes) anchored \
                            to a symbol name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": {"type": "string", "description": "Symbol name, e.g. refresh_session"}
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "repo_history",
            "description": "Return git commit units that changed a symbol, newest context first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": {"type": "string", "description": "Symbol name, e.g. refresh_session"}
                },
                "required": ["symbol"]
            }
        }
    ])
}

fn result_response(id: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: serde_json::Value, code: i64, message: String) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn text_result(text: String) -> serde_json::Value {
    serde_json::json!({"content": [{"type": "text", "text": text}]})
}

pub fn handle_message(
    store: &Store,
    repo_id: RepoId,
    embedder: Option<&dyn Embedder>,
    message: &serde_json::Value,
) -> Option<serde_json::Value> {
    let id = message.get("id")?.clone();
    let method = message.get("method").and_then(|value| value.as_str())?;
    let params = message
        .get("params")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(|value| value.as_str())
                .unwrap_or(PROTOCOL_VERSION);
            Some(result_response(
                id,
                serde_json::json!({
                    "protocolVersion": requested,
                    "capabilities": {"tools": {}},
                    "serverInfo": {
                        "name": "snoop",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            ))
        }
        "ping" => Some(result_response(id, serde_json::json!({}))),
        "tools/list" => Some(result_response(
            id,
            serde_json::json!({"tools": tool_definitions()}),
        )),
        "tools/call" => {
            let name = params.get("name").and_then(|value| value.as_str());
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            match name {
                Some(tool) => Some(call_tool(store, repo_id, embedder, id, tool, &arguments)),
                None => Some(error_response(
                    id,
                    -32602,
                    "tools/call requires a tool name".to_string(),
                )),
            }
        }
        other => Some(error_response(
            id,
            -32601,
            format!("unknown method: {other}"),
        )),
    }
}

fn call_tool(
    store: &Store,
    repo_id: RepoId,
    embedder: Option<&dyn Embedder>,
    id: serde_json::Value,
    tool: &str,
    arguments: &serde_json::Value,
) -> serde_json::Value {
    let outcome: Result<serde_json::Value, String> = match tool {
        "get_repo_context" => {
            let Some(query_text) = arguments.get("query").and_then(|value| value.as_str()) else {
                return error_response(id, -32602, "get_repo_context requires query".to_string());
            };
            let max_tokens = arguments
                .get("max_tokens")
                .and_then(|value| value.as_u64())
                .map(|value| value.clamp(100, MAX_TOKENS_LIMIT as u64) as usize)
                .unwrap_or(DEFAULT_MAX_TOKENS);
            let query_options = QueryOptions {
                max_tokens,
                channels: QueryChannels::for_embedder(embedder),
                ..QueryOptions::default()
            };
            query(store, repo_id, embedder, query_text, &query_options)
                .map(|report| serde_json::to_value(&report.packet).unwrap_or_default())
                .map_err(|error| error.to_string())
        }
        "repo_symbol_context" => required_symbol(arguments).and_then(|symbol| {
            symbol_context_entries(store, repo_id, &symbol)
                .map(|entries| serde_json::json!(entries))
                .map_err(|error| error.to_string())
        }),
        "repo_history" => required_symbol(arguments).and_then(|symbol| {
            history_entries(store, repo_id, &symbol)
                .map(|entries| serde_json::json!(entries))
                .map_err(|error| error.to_string())
        }),
        other => return error_response(id, -32602, format!("unknown tool: {other}")),
    };
    match outcome {
        Ok(value) => result_response(
            id,
            text_result(serde_json::to_string_pretty(&value).unwrap_or_default()),
        ),
        Err(message) => result_response(id, {
            let mut result = text_result(message);
            result["isError"] = serde_json::json!(true);
            result
        }),
    }
}

fn required_symbol(arguments: &serde_json::Value) -> Result<String, String> {
    arguments
        .get("symbol")
        .and_then(|value| value.as_str())
        .map(|symbol| symbol.to_string())
        .ok_or_else(|| "this tool requires a symbol".to_string())
}

pub fn serve<R: BufRead, W: Write>(
    store: &Store,
    repo_id: RepoId,
    embedder: Option<&dyn Embedder>,
    input: &mut R,
    output: &mut W,
) -> std::io::Result<()> {
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(message) => handle_message(store, repo_id, embedder, &message),
            Err(error) => Some(error_response(
                serde_json::Value::Null,
                -32700,
                format!("parse error: {error}"),
            )),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut *output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_produce_no_response() {
        let notification =
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let store = Store::open_in_memory().unwrap();
        let embedder = crate::inference::MockEmbedder::new("mock-v1");
        assert!(handle_message(&store, RepoId(1), Some(&embedder), &notification).is_none());
    }
}
