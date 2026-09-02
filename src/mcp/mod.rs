//! MCP stdio protocol: tool definitions, JSON-RPC framing, and tool dispatch.
//! The serve loop (worker pool, embed deadline, breaker) lives in `serve`.

mod serve;

pub use serve::{serve, ServeConfig};

use serve::BoundedEmbed;

use crate::core::SourceKind;
use crate::inference::Embedder;
use crate::runtime::{query, query_with_vector, QueryChannels, QueryOptions};
use crate::store::Store;
use std::collections::HashSet;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const DEFAULT_MAX_TOKENS: usize = 6_000;
const MAX_TOKENS_LIMIT: usize = 32_000;
use crate::store::ANCHOR_LOOKUP_LIMIT;

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;

pub fn symbol_context_entries(
    store: &Store,
    symbol: &str,
) -> Result<(Vec<serde_json::Value>, usize), Error> {
    symbol_context_entries_excluding(store, symbol, &HashSet::new(), QueryOptions::default().now)
}

pub fn symbol_context_entries_excluding(
    store: &Store,
    symbol: &str,
    excluded: &HashSet<i64>,
    now: i64,
) -> Result<(Vec<serde_json::Value>, usize), Error> {
    let mut report = Vec::new();
    let mut ids: Vec<i64> = store
        .unit_ids_for_anchor("symbol", symbol)?
        .into_iter()
        .filter(|id| !excluded.contains(id))
        .collect();
    let more = ids.len().saturating_sub(ANCHOR_LOOKUP_LIMIT);
    ids.truncate(ANCHOR_LOOKUP_LIMIT);
    for id in ids {
        if let Some(unit) = store.unit_by_id(id)? {
            let mut entry = serde_json::json!({
                "unit_id": id,
                "source_kind": unit.source_kind.as_str(),
                "locator": unit.locator,
                "routing_text": unit.routing_text,
            });
            if matches!(
                unit.source_kind,
                SourceKind::GitCommit | SourceKind::AgentSession
            ) {
                if let Some(timestamp) = unit.timestamp {
                    entry["timestamp"] =
                        serde_json::json!(crate::metadata::timestamp::render(timestamp, now));
                }
            }
            if unit.source_kind == SourceKind::GitCommit {
                entry["evidence_text"] = serde_json::json!(unit.evidence_text);
            }
            report.push(entry);
        }
    }
    Ok((report, more))
}

fn tool_definitions() -> serde_json::Value {
    serde_json::json!([
        {
            "name": "context",
            "description": "Get relevant repository context across code, docs, git history, and prior agent work.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What you need to understand."
                    },
                    "max_tokens": {
                        "type": "integer",
                        "description": "Maximum context to return. Default 6000.",
                        "default": DEFAULT_MAX_TOKENS
                    },
                    "exclude_sessions": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Session IDs whose already-visible episodes must be excluded."
                    }
                },
                "required": ["query"]
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

/// Outcome of one tool invocation before JSON-RPC framing.
enum ToolSuccess {
    Payload(serde_json::Value),
    /// Served without the embedder within its deadline; payload is complete
    /// but vector channels were dropped.
    Degraded(serde_json::Value),
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
    if tool != "context" {
        return Err(ToolFailure::Usage {
            code: -32602,
            message: format!("unknown tool: {tool}"),
        });
    }
    let Some(query_text) = arguments.get("query").and_then(|value| value.as_str()) else {
        return Err(ToolFailure::Usage {
            code: -32602,
            message: "context requires query".to_string(),
        });
    };
    let max_tokens = arguments
        .get("max_tokens")
        .and_then(|value| value.as_u64())
        .map(|value| value.clamp(100, MAX_TOKENS_LIMIT as u64) as usize)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    let channels = QueryChannels::for_embedder(embedder);
    let exclude_unit_ids = excluded_session_units(store, arguments)?;
    let options = QueryOptions {
        max_tokens,
        channels,
        exclude_unit_ids: exclude_unit_ids.clone(),
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
            exclude_unit_ids,
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

fn excluded_session_units(
    store: &Store,
    arguments: &serde_json::Value,
) -> Result<HashSet<i64>, ToolFailure> {
    let Some(value) = arguments.get("exclude_sessions") else {
        return Ok(HashSet::new());
    };
    let Some(sessions) = value.as_array() else {
        return Err(ToolFailure::Usage {
            code: -32602,
            message: "exclude_sessions must be an array of strings".to_string(),
        });
    };
    let mut excluded = HashSet::new();
    for value in sessions {
        let Some(session) = value.as_str() else {
            return Err(ToolFailure::Usage {
                code: -32602,
                message: "exclude_sessions must be an array of strings".to_string(),
            });
        };
        excluded.extend(
            store
                .unit_ids_for_anchor("session", session)
                .map_err(|error| ToolFailure::Error(error.to_string()))?,
        );
    }
    Ok(excluded)
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
            let payload = match success {
                ToolSuccess::Payload(payload) | ToolSuccess::Degraded(payload) => payload,
            };
            result_response(
                id,
                text_result(serde_json::to_string_pretty(&payload).unwrap_or_default()),
            )
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
