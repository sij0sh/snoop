//! Serve loop (audit fix-c1 / AP-1): bounded worker pool, server-side embed
//! deadline, circuit breaker, and lexical-only degraded answers.
//!
//! The reader thread keeps consuming stdin while tool calls run on workers;
//! JSON-RPC responses may be written out of order. The control plane
//! (initialize/ping/tools-list/parse errors) never touches a worker or the
//! embedder, so a stuck embedder cannot delay ping.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use super::{ToolFailure, ToolSuccess, control_response, dispatch_tool, error_response,
            result_response, text_result};
use crate::inference::Embedder;
use crate::store::{Store, StoreOpenError};

/// Serve-loop tuning. Defaults live in `main.rs` via env:
/// `SNOOP_MCP_WORKERS` (default 4) and `SNOOP_EMBED_DEADLINE_MS` (default 2000).
/// Rollback: workers=1 with a huge deadline restores the sequential behavior.
pub struct ServeConfig {
    /// Opens one `Store` per worker thread (a `rusqlite` connection is not
    /// `Sync`, so the pool cannot share one handle).
    pub open_store: Arc<dyn Fn() -> Result<Store, StoreOpenError> + Send + Sync>,
    pub embedder: Option<Arc<dyn Embedder>>,
    pub workers: usize,
    pub embed_deadline: Duration,
}

/// Consecutive embed deadlines before query embeds are skipped outright.
const BREAKER_TRIP_AFTER: u32 = 3;
/// How long the open breaker serves lexical-only before trying embeds again.
const BREAKER_COOLDOWN: Duration = Duration::from_secs(30);

/// Circuit breaker over query-embed deadlines: three consecutive deadline
/// hits open the breaker for a cooldown window during which requests degrade
/// to lexical-only immediately instead of paying the deadline each time.
#[derive(Default)]
pub(crate) struct Breaker {
    consecutive_deadlines: u32,
    open_until: Option<Instant>,
}

impl Breaker {
    fn allow(&self) -> bool {
        self.open_until.is_none_or(|until| Instant::now() >= until)
    }

    fn record_success(&mut self) {
        self.consecutive_deadlines = 0;
        self.open_until = None;
    }

    fn record_deadline(&mut self) {
        self.consecutive_deadlines += 1;
        if self.consecutive_deadlines >= BREAKER_TRIP_AFTER {
            eprintln!(
                "snoop mcp: embed breaker open for {}s after {} deadline hits",
                BREAKER_COOLDOWN.as_secs(),
                self.consecutive_deadlines
            );
            self.open_until = Some(Instant::now() + BREAKER_COOLDOWN);
            self.consecutive_deadlines = 0;
        }
    }
}

/// Deadline-bounded embed plumbing for the serve loop. `None` in
/// `dispatch_tool` reproduces the historical synchronous behavior.
pub(crate) struct BoundedEmbed<'a> {
    embedder: &'a Arc<dyn Embedder>,
    deadline: Duration,
    breaker: &'a Mutex<Breaker>,
}

pub(crate) enum BoundedEmbedOutcome {
    Vector(Vec<f32>),
    /// Deadline hit or breaker open: caller serves lexical-only.
    Degrade,
    /// Embedder failed (single attempt, no retry): surfaced to the client.
    Failed(super::Error),
}

pub(crate) fn bounded_embed_query(bounded: &BoundedEmbed<'_>, text: &str) -> BoundedEmbedOutcome {
    if !bounded.breaker.lock().expect("breaker").allow() {
        return BoundedEmbedOutcome::Degrade;
    }
    // A timed-out embed thread lives on until its HTTP request gives up; the
    // breaker trips after repeated timeouts so leaked threads stay bounded.
    let (sender, receiver) = mpsc::channel();
    let embedder = Arc::clone(bounded.embedder);
    let text = text.to_string();
    thread::spawn(move || {
        let _ = sender.send(embedder.embed_query(&text));
    });
    match receiver.recv_timeout(bounded.deadline) {
        Ok(Ok(vector)) => {
            bounded.breaker.lock().expect("breaker").record_success();
            BoundedEmbedOutcome::Vector(vector)
        }
        Ok(Err(error)) => BoundedEmbedOutcome::Failed(error),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            bounded.breaker.lock().expect("breaker").record_deadline();
            BoundedEmbedOutcome::Degrade
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            unreachable!("embed thread always sends a result")
        }
    }
}

fn worker_response(
    store: &Store,
    embedder: Option<&Arc<dyn Embedder>>,
    breaker: &Mutex<Breaker>,
    embed_deadline: Duration,
    message: &serde_json::Value,
) -> serde_json::Value {
    let id = message
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let params = message
        .get("params")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let Some(tool) = params.get("name").and_then(|value| value.as_str()) else {
        return error_response(id, -32602, "tools/call requires a tool name".to_string());
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let outcome = match embedder {
        Some(embedder) => {
            let bounded = BoundedEmbed {
                embedder,
                deadline: embed_deadline,
                breaker,
            };
            dispatch_tool(
                store,
                Some(embedder.as_ref()),
                Some(&bounded),
                tool,
                &arguments,
            )
        }
        None => dispatch_tool(store, None, None, tool, &arguments),
    };
    match outcome {
        Ok(ToolSuccess::Payload(payload)) => result_response(
            id,
            text_result(serde_json::to_string_pretty(&payload).unwrap_or_default()),
        ),
        Ok(ToolSuccess::Degraded(payload)) => result_response(id, {
            let mut result =
                text_result(serde_json::to_string_pretty(&payload).unwrap_or_default());
            // Additive response field: the client can see the answer is
            // lexical-only. Visible degraded mode, not a hidden failure.
            result["degraded"] = serde_json::json!(true);
            result
        }),
        Err(ToolFailure::Usage { code, message }) => error_response(id, code, message),
        Err(ToolFailure::Error(message)) => result_response(id, {
            let mut result = text_result(message);
            result["isError"] = serde_json::json!(true);
            result
        }),
    }
}

pub fn serve<R, W>(config: ServeConfig, input: R, mut output: W) -> std::io::Result<()>
where
    R: BufRead + Send,
    W: Write,
{
    const POLL: Duration = Duration::from_millis(1);

    let (results_tx, results_rx) = mpsc::channel::<serde_json::Value>();
    let (jobs_tx, jobs_rx) = mpsc::channel::<serde_json::Value>();
    let jobs_rx = Arc::new(Mutex::new(jobs_rx));
    let breaker = Arc::new(Mutex::new(Breaker::default()));

    fn write_response<W: Write>(
        output: &mut W,
        response: &serde_json::Value,
    ) -> std::io::Result<()> {
        serde_json::to_writer(&mut *output, response)?;
        output.write_all(b"\n")?;
        output.flush()
    }

    // The reader runs in a scope so `input` can borrow; worker captures are
    // owned and would equally work as detached threads.
    thread::scope(|scope| -> std::io::Result<()> {
        // Reader thread: stdin keeps being consumed while tool calls run.
        let (inbound_tx, inbound_rx) = mpsc::channel::<Result<serde_json::Value, String>>();
        scope.spawn(move || {
            let mut reader = input;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let parsed = serde_json::from_str::<serde_json::Value>(&line)
                            .map_err(|error| error.to_string());
                        if inbound_tx.send(parsed).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        for _ in 0..config.workers.max(1) {
            let jobs_rx = Arc::clone(&jobs_rx);
            let results_tx = results_tx.clone();
            let breaker = Arc::clone(&breaker);
            let embedder = config.embedder.clone();
            let open_store = Arc::clone(&config.open_store);
            let embed_deadline = config.embed_deadline;
            scope.spawn(move || {
                let store = (open_store)();
                loop {
                    let message = {
                        let receiver = jobs_rx.lock().expect("job queue");
                        receiver.recv()
                    };
                    let Ok(message) = message else {
                        break;
                    };
                    let response = match &store {
                        Ok(store) => {
                            worker_response(store, embedder.as_ref(), &breaker, embed_deadline, &message)
                        }
                        Err(error) => error_response(
                            message.get("id").cloned().unwrap_or(serde_json::Value::Null),
                            -32603,
                            format!("store open failed: {error}"),
                        ),
                    };
                    if results_tx.send(response).is_err() {
                        break;
                    }
                }
            });
        }
        drop(results_tx);

        // Control plane stays inline: a stuck embedder cannot delay ping
        // (probe invariant: ping completes in < 10 ms under embed lag).
        let mut outstanding = 0usize;
        let outcome = 'serve: loop {
            while let Ok(response) = results_rx.try_recv() {
                if write_response(&mut output, &response).is_err() {
                    break 'serve Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "failed to write MCP response",
                    ));
                }
                outstanding -= 1;
            }
            match inbound_rx.recv_timeout(POLL) {
                Ok(Ok(message)) => {
                    let is_call = message.get("method").and_then(|value| value.as_str())
                        == Some("tools/call")
                        && message.get("id").is_some();
                    if is_call {
                        outstanding += 1;
                        let _ = jobs_tx.send(message);
                    } else if let Some(id) = message.get("id") {
                        // Notifications (no id) get no response, matching
                        // `handle_message`.
                        if let Some(response) = message
                            .get("method")
                            .and_then(|value| value.as_str())
                            .and_then(|method| {
                                control_response(method, id, message.get("params"))
                            })
                        {
                            if write_response(&mut output, &response).is_err() {
                                break 'serve Err(std::io::Error::new(
                                    std::io::ErrorKind::BrokenPipe,
                                    "failed to write MCP response",
                                ));
                            }
                        }
                    }
                }
                Ok(Err(parse_error)) => {
                    let response = error_response(
                        serde_json::Value::Null,
                        -32700,
                        format!("parse error: {parse_error}"),
                    );
                    if write_response(&mut output, &response).is_err() {
                        break 'serve Err(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "failed to write MCP response",
                        ));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // EOF: answers for dispatched calls are still written
                    // (same completion guarantee as the sequential loop).
                    while outstanding > 0 {
                        match results_rx.recv() {
                            Ok(response) => {
                                if write_response(&mut output, &response).is_err() {
                                    break;
                                }
                                outstanding -= 1;
                            }
                            Err(_) => break,
                        }
                    }
                    break 'serve Ok(());
                }
            }
        };
        // Release the workers so the scope can join.
        drop(jobs_tx);
        outcome
    })
}
