use std::io::BufRead;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use snoop::core::Repository;
use snoop::inference::{Embedder, LlamaServerEmbedder, MockEmbedder};
use snoop::ingest::{index_repository_bounded, scanner, LockedError};
use snoop::runtime::{query, QueryChannels, QueryOptions};
use snoop::store::Store;

const DEFAULT_ENSURE_TIMEOUT_SECS: u64 = 120;

#[derive(Parser)]
#[command(name = "snoop", about = "Local repository context compiler")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    Index {
        path: Option<PathBuf>,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    Ensure {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        timeout: Option<u64>,
    },
    Status {
        #[arg(long)]
        db: Option<PathBuf>,
    },
    Query {
        query: String,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = 6_000)]
        tokens: usize,
        #[arg(long, default_value_t = 25)]
        top: usize,
        #[arg(long)]
        explain: bool,
        #[arg(long)]
        evidence_only: bool,
    },
    Inspect {
        target: String,
        value: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    History {
        symbol: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    Sessions {
        symbol: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    Mcp {
        #[arg(long)]
        db: Option<PathBuf>,
    },
}

fn db_path(path: Option<PathBuf>) -> PathBuf {
    path.or_else(|| std::env::var_os("SNOOP_DB").map(PathBuf::from))
        .unwrap_or_else(|| {
            let home = std::env::home_dir().expect("cannot resolve home directory");
            home.join(".snoop").join("snoop.db")
        })
}

fn open_store(path: &Path) -> Result<Store, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(Store::open(path)?)
}

fn bound_repository(store: &Store) -> Result<Repository, Box<dyn std::error::Error + Send + Sync>> {
    store
        .repository()?
        .ok_or_else(|| "index a repository first".into())
}

fn embedder() -> Option<Box<dyn Embedder>> {
    let Ok(url) = std::env::var("SNOOP_EMBED_URL") else {
        return None;
    };
    if url == "mock" {
        Some(Box::new(MockEmbedder::new(
            snoop::inference::MOCK_MODEL_VERSION,
        )))
    } else {
        let version = std::env::var("SNOOP_EMBED_VERSION")
            .unwrap_or_else(|_| "Qwen3-Embedding-0.6B-Q8_0".to_string());
        Some(Box::new(LlamaServerEmbedder::new(&url, &version)))
    }
}

fn print_ensure_error(message: String) -> ! {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "status": "error",
            "error": message,
        }))
        .unwrap_or_else(|_| "{\"status\": \"error\"}".to_string())
    );
    std::process::exit(1)
}

/// `Stdin` adapter implementing `BufRead` that is `Send`: the std `StdinLock`
/// guard is not `Send`, but the serve loop reads lines on its reader thread.
struct SendBufStdin {
    stdin: std::io::Stdin,
    buffer: Vec<u8>,
    position: usize,
}

impl std::io::Read for SendBufStdin {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let available = self.fill_buf()?;
        let take = available.len().min(buf.len());
        buf[..take].copy_from_slice(&available[..take]);
        self.consume(take);
        Ok(take)
    }
}
impl std::io::BufRead for SendBufStdin {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.position >= self.buffer.len() {
            self.buffer.clear();
            self.position = 0;
            self.stdin.lock().read_until(b'\n', &mut self.buffer)?;
        }
        Ok(&self.buffer[self.position..])
    }

    fn consume(&mut self, amount: usize) {
        self.position = (self.position + amount).min(self.buffer.len());
    }
}

fn retrieval_mode(embedder: Option<&dyn Embedder>) -> (&'static str, Option<String>) {
    match embedder {
        Some(embedder) => ("hybrid", Some(embedder.model_version().to_string())),
        None => ("lexical+anchors", None),
    }
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match cli.command {
        Command::Init { path, db } => {
            let mut store = open_store(&db_path(db))?;
            let root = scanner::repository_root(&path)?;
            let repository = store.bind_repository(&root.to_string_lossy())?;
            let outcome = index_repository_bounded(&mut store, &root, None, None)?;
            let skipped = if outcome.skipped_sources > 0 {
                format!(", {} skipped", outcome.skipped_sources)
            } else {
                String::new()
            };
            println!(
                "initialized repository at {} ({} sources{})",
                repository.root_path,
                outcome.changed_sources + outcome.unchanged_sources,
                skipped
            );
        }
        Command::Index { path, db } => {
            let mut store = open_store(&db_path(db))?;
            let root = match path {
                Some(path) => scanner::repository_root(&path)?,
                None => PathBuf::from(bound_repository(&store)?.root_path),
            };
            store.bind_repository(&root.to_string_lossy())?;
            let embedder = embedder();
            let outcome = index_repository_bounded(&mut store, &root, embedder.as_deref(), None)?;

            let outcome_json = serde_json::to_value(&outcome)?;
            println!("{}", serde_json::to_string_pretty(&outcome_json)?);
        }
        Command::Ensure { path, db, timeout } => {
            let started = std::time::Instant::now();
            let timeout_secs = timeout.unwrap_or(DEFAULT_ENSURE_TIMEOUT_SECS);
            let deadline = started + std::time::Duration::from_secs(timeout_secs);
            let root = match scanner::repository_root(&path) {
                Ok(root) => root,
                Err(error) => print_ensure_error(error.to_string()),
            };
            let mut store = match open_store(&db_path(db)) {
                Ok(store) => store,
                Err(error) => print_ensure_error(error.to_string()),
            };
            let _repository = match store.bind_repository(&root.to_string_lossy()) {
                Ok(repository) => repository,
                Err(error) => print_ensure_error(error.to_string()),
            };
            let embedder = embedder();
            let result =
                index_repository_bounded(&mut store, &root, embedder.as_deref(), Some(deadline));
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) if error.is::<LockedError>() => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "status": "locked",
                        }))?
                    );
                    return Ok(());
                }
                Err(error) => print_ensure_error(error.to_string()),
            };
            let status = if outcome.timed_out {
                "timeout"
            } else if outcome.changed_sources == 0
                && outcome.deleted_sources == 0
                && outcome.embedded == 0
            {
                "up-to-date"
            } else {
                "refreshed"
            };
            let report = match status {
                "refreshed" | "up-to-date" => serde_json::json!({
                    "status": status,
                    "outcome": outcome,
                }),
                _ => serde_json::json!({ "status": status }),
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Status { db } => {
            let store = open_store(&db_path(db))?;
            bound_repository(&store)?;
            let mut status = serde_json::to_value(store.stats()?)?;
            let embedder = embedder();
            let (mode, model) = retrieval_mode(embedder.as_deref());
            status["retrieval_mode"] = serde_json::json!(mode);
            if let Some(model) = model {
                status["embedding_model"] = serde_json::json!(model);
            }
            let vector_models: Vec<serde_json::Value> = store
                .vector_models()?
                .into_iter()
                .map(|(model, vectors)| serde_json::json!({"model": model, "vectors": vectors}))
                .collect();
            if !vector_models.is_empty() {
                status["vector_models"] = serde_json::Value::Array(vector_models);
            }
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Command::Query {
            query: query_text,
            db,
            tokens,
            top,
            explain,
            evidence_only,
        } => {
            let store = open_store(&db_path(db))?;
            bound_repository(&store)?;
            let embedder = embedder();
            let channels = if evidence_only {
                if embedder.is_some() {
                    QueryChannels::evidence_only()
                } else {
                    QueryChannels::evidence_lexical_only()
                }
            } else {
                QueryChannels::for_embedder(embedder.as_deref())
            };
            let report = query(
                &store,
                embedder.as_deref(),
                &query_text,
                &QueryOptions {
                    channels,
                    top_n: top,
                    max_tokens: tokens,
                    diagnostics: explain,
                },
            )?;
            if let Some(debug) = &report.debug {
                eprintln!("{}", serde_json::to_string_pretty(debug)?);
            }
            println!("{}", serde_json::to_string_pretty(&report.packet)?);
        }
        Command::Inspect { target, value, db } => {
            let store = open_store(&db_path(db))?;
            bound_repository(&store)?;
            match target.as_str() {
                "unit" => {
                    let id: i64 = value.parse().map_err(|_| "unit id must be numeric")?;
                    let unit = store.unit_by_id(id)?.ok_or_else(
                        || -> Box<dyn std::error::Error + Send + Sync> {
                            format!("unit {id} not found").into()
                        },
                    )?;
                    let anchors = store
                        .anchors_for_unit(id)?
                        .into_iter()
                        .map(|anchor| {
                            serde_json::json!({
                                "kind": anchor.kind.as_str(),
                                "value": anchor.value,
                                "relationship": anchor.relationship,
                            })
                        })
                        .collect::<Vec<_>>();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "unit": unit,
                            "anchors": anchors,
                        }))?
                    );
                }
                "symbol" => {
                    let entries = snoop::mcp::symbol_context_entries(&store, &value)?;
                    println!("{}", serde_json::to_string_pretty(&entries)?);
                }
                other => return Err(format!("unknown inspect target: {other}").into()),
            }
        }
        Command::History { symbol, db } => {
            let store = open_store(&db_path(db))?;
            bound_repository(&store)?;
            let entries = snoop::mcp::history_entries(&store, &symbol)?;
            println!("{}", serde_json::to_string_pretty(&entries)?);
        }
        Command::Sessions { symbol, db } => {
            let store = open_store(&db_path(db))?;
            bound_repository(&store)?;

            let mut defining_files = std::collections::HashSet::new();
            for id in store.units_for_anchor("symbol", &symbol, 64)? {
                if let Some(unit) = store.unit_by_id(id)? {
                    if unit.source_kind == snoop::core::SourceKind::Code {
                        for anchor in store.anchors_for_unit(id)? {
                            if anchor.kind.as_str() == "file" {
                                defining_files.insert(anchor.value);
                            }
                        }
                    }
                }
            }
            let mut episodes = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for file in &defining_files {
                for id in store.units_for_anchor("file", file, 64)? {
                    if !seen.insert(id) {
                        continue;
                    }
                    if let Some(unit) = store.unit_by_id(id)? {
                        if unit.source_kind == snoop::core::SourceKind::AgentSession {
                            episodes.push(serde_json::json!({
                                "unit_id": id,
                                "locator": unit.locator,
                                "timestamp": unit.timestamp,
                                "evidence_text": unit.evidence_text,
                            }));
                        }
                    }
                }
            }
            println!("{}", serde_json::to_string_pretty(&episodes)?);
        }
        Command::Mcp { db } => {
            let db_path = db_path(db);
            let store = open_store(&db_path)?;
            bound_repository(&store)?;
            // deadline. workers=1 with a huge deadline restores old behavior.
            let embedder: Option<std::sync::Arc<dyn Embedder>> =
                embedder().map(std::sync::Arc::from);
            let workers = std::env::var("SNOOP_MCP_WORKERS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(4)
                .max(1);
            let embed_deadline = std::time::Duration::from_millis(
                std::env::var("SNOOP_EMBED_DEADLINE_MS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(2000)
                    .max(1),
            );
            snoop::mcp::serve(
                snoop::mcp::ServeConfig {
                    open_store: std::sync::Arc::new(move || snoop::store::Store::open(&db_path)),
                    embedder,
                    workers,
                    embed_deadline,
                },
                SendBufStdin {
                    stdin: std::io::stdin(),
                    buffer: Vec::new(),
                    position: 0,
                },
                std::io::stdout().lock(),
            )?;
        }
    }
    Ok(())
}
