//! MCP stdio protocol: tool definitions, JSON-RPC framing, and tool dispatch.
//! The serve loop (worker pool, embed deadline, breaker) lives in `serve`.

mod serve;

pub use serve::{serve, ServeConfig};

use serve::BoundedEmbed;

use crate::core::SourceKind;
use crate::inference::Embedder;
use crate::runtime::{query, query_with_vector, QueryChannels, QueryOptions};
use crate::store::Store;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const DEFAULT_MAX_TOKENS: usize = 6_000;
const MAX_TOKENS_LIMIT: usize = 32_000;
use crate::store::ANCHOR_LOOKUP_LIMIT;

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;

pub fn symbol_context_entries(
    store: &Store,
    symbol: &str,
) -> Result<(Vec<serde_json::Value>, usize), Error> {
    let mut report = Vec::new();
    let (ids, more) = store.units_for_anchor("symbol", symbol, ANCHOR_LOOKUP_LIMIT)?;
    for id in ids {
        if let Some(unit) = store.unit_by_id(id)? {
            let mut entry = serde_json::json!({
                "unit_id": id,
                "source_kind": unit.source_kind.as_str(),
                "locator": unit.locator,
                "routing_text": unit.routing_text,
            });
            if unit.source_kind == SourceKind::GitCommit {
                entry["timestamp"] = serde_json::json!(unit.timestamp);
                entry["evidence_text"] = serde_json::json!(unit.evidence_text);
            }
            report.push(entry);
        }
    }
    Ok((report, more))
}

pub fn history_entries(
    store: &Store,
    symbol: &str,
) -> Result<(Vec<serde_json::Value>, usize), Error> {
    let mut history = Vec::new();
    let (ids, more) = store.units_for_anchor("symbol", symbol, ANCHOR_LOOKUP_LIMIT)?;
    for id in ids {
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
    Ok((history, more))
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
                            to a symbol name; commit units carry timestamp and evidence_text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": {"type": "string", "description": "Symbol name, e.g. refresh_session"}
                },
                "required": ["symbol"]
            }
        },
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

/// Outcome of one tool invocation before JSON-RPC framing.
enum ToolSuccess {
    Payload(serde_json::Value),
    /// Served without the embedder within its deadline; payload is complete
    /// but vector channels were dropped.
    Degraded(serde_json::Value),
    /// The anchor lookup hit the display cap; the payload holds the oldest
    /// page and the response carries `truncated: true` (defect-audit c6).
    Truncated(serde_json::Value),
}

enum ToolFailure {
    Usage { code: i64, message: String },
    Error(String),
}

fn dispatch_tool(
    store: &Store,
    embedder: Option<&dyn Embedder>,
    bounded: Option<&BoundedEmbed<'_>>,
    tool: &str,
    arguments: &serde_json::Value,
) -> Result<ToolSuccess, ToolFailure> {
    match tool {
        "get_repo_context" => {
            let Some(query_text) = arguments.get("query").and_then(|value| value.as_str()) else {
                return Err(ToolFailure::Usage {
                    code: -32602,
                    message: "get_repo_context requires query".to_string(),
                });
            };
            let max_tokens = arguments
                .get("max_tokens")
                .and_then(|value| value.as_u64())
                .map(|value| value.clamp(100, MAX_TOKENS_LIMIT as u64) as usize)
                .unwrap_or(DEFAULT_MAX_TOKENS);
            let channels = QueryChannels::for_embedder(embedder);
            let options = QueryOptions {
                max_tokens,
                channels,
                ..QueryOptions::default()
            };
            // Audit fix-c1: the query embedding is fetched under the serve
            // deadline up front and reused, so the query itself never embeds.
            let mut query_vector = None;
            let mut degraded = false;
            if channels.has_vector_channels() {
                if let Some(bounded) = bounded {
                    match serve::bounded_embed_query(bounded, query_text) {
                        serve::BoundedEmbedOutcome::Vector(vector) => query_vector = Some(vector),
                        serve::BoundedEmbedOutcome::Degrade => degraded = true,
                        serve::BoundedEmbedOutcome::Failed(error) => {
                            return Err(ToolFailure::Error(error.to_string()))
                        }
                    }
                }
            }
            if degraded {
                // Deadline hit or breaker open: answer from lexical channels
                // only; the response carries `degraded: true`.
                let lexical = QueryOptions {
                    max_tokens,
                    channels: QueryChannels::for_embedder(None),
                    ..QueryOptions::default()
                };
                let report = query(store, None, query_text, &lexical)
                    .map_err(|error| ToolFailure::Error(error.to_string()))?;
                eprintln!("snoop mcp: degraded lexical-only answer (embed deadline/breaker)");
                return Ok(ToolSuccess::Degraded(
                    serde_json::to_value(&report.packet).unwrap_or_default(),
                ));
            }
            let report = query_with_vector(store, embedder, query_text, &options, query_vector)
                .map_err(|error| ToolFailure::Error(error.to_string()))?;
            Ok(ToolSuccess::Payload(
                serde_json::to_value(&report.packet).unwrap_or_default(),
            ))
        }
        "repo_symbol_context" => {
            let symbol = required_symbol(arguments).map_err(ToolFailure::Error)?;
            symbol_context_entries(store, &symbol)
                .map(|(entries, more)| {
                    if more > 0 {
                        ToolSuccess::Truncated(serde_json::json!(entries))
                    } else {
                        ToolSuccess::Payload(serde_json::json!(entries))
                    }
                })
                .map_err(|error| ToolFailure::Error(error.to_string()))
        }
        other => Err(ToolFailure::Usage {
            code: -32602,
            message: format!("unknown tool: {other}"),
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

pub fn handle_message(
    store: &Store,
    embedder: Option<&dyn Embedder>,
    message: &serde_json::Value,
) -> Option<serde_json::Value> {
    let id = message.get("id")?.clone();
    let method = message.get("method").and_then(|value| value.as_str())?;
    if method != "tools/call" {
        return control_response(method, &id, message.get("params"));
    }
    let params = message
        .get("params")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let name = params.get("name").and_then(|value| value.as_str());
    match name {
        Some(tool) => {
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            Some(frame_tool_result(
                id,
                dispatch_tool(store, embedder, None, tool, &arguments),
            ))
        }
        None => Some(error_response(
            id,
            -32602,
            "tools/call requires a tool name".to_string(),
        )),
    }
}

fn frame_tool_result(
    id: serde_json::Value,
    outcome: Result<ToolSuccess, ToolFailure>,
) -> serde_json::Value {
    match outcome {
        Ok(success) => {
            let (payload, truncated) = match success {
                ToolSuccess::Payload(payload) => (payload, false),
                ToolSuccess::Degraded(payload) => (payload, false),
                ToolSuccess::Truncated(payload) => (payload, true),
            };
            let mut result =
                text_result(serde_json::to_string_pretty(&payload).unwrap_or_default());
            if truncated {
                // Additive response field: the anchor lookup hit the display
                // cap and only the oldest page is served (defect-audit c6).
                result["truncated"] = serde_json::json!(true);
            }
            result_response(id, result)
        }
        Err(ToolFailure::Usage { code, message }) => error_response(id, code, message),
        Err(ToolFailure::Error(message)) => result_response(id, {
            let mut result = text_result(message);
            result["isError"] = serde_json::json!(true);
            result
        }),
    }
}

fn control_response(
    method: &str,
    id: &serde_json::Value,
    params: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    match method {
        "initialize" => {
            let requested = params
                .and_then(|value| value.get("protocolVersion"))
                .and_then(|value| value.as_str())
                .unwrap_or(PROTOCOL_VERSION);
            Some(result_response(
                id.clone(),
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
        "ping" => Some(result_response(id.clone(), serde_json::json!({}))),
        "tools/list" => Some(result_response(
            id.clone(),
            serde_json::json!({"tools": tool_definitions()}),
        )),
        "tools/call" => None,
        other => Some(error_response(
            id.clone(),
            -32601,
            format!("unknown method: {other}"),
        )),
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
        assert!(handle_message(&store, Some(&embedder), &notification).is_none());
    }
}
