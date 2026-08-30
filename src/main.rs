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
        #[arg(long)]
        repo: Option<PathBuf>,
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
        #[arg(long)]
        repo: Option<PathBuf>,
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
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    Inspect {
        target: String,
        value: String,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    History {
        symbol: String,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    Sessions {
        symbol: String,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    Mcp {
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

fn db_path(path: Option<PathBuf>) -> PathBuf {
    path.or_else(|| std::env::var_os("SNOOP_DB").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(".snoop.db"))
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

fn select_repository(
    store: &Store,
    explicit: Option<&Path>,
) -> Result<Repository, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(path) = explicit {
        let root = scanner::repository_root(path)?;
        return store
            .repository_by_root(&root.to_string_lossy())?
            .ok_or_else(|| format!("repository is not indexed: {}", root.display()).into());
    }
    let current = scanner::repository_root(&std::env::current_dir()?)?;
    if let Some(repository) = store.repository_by_root(&current.to_string_lossy())? {
        return Ok(repository);
    }
    if store.stats()?.repositories == 1 {
        return store
            .first_repository()?
            .ok_or_else(|| "index a repository first".into());
    }
    Err("select a repository with --repo".into())
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
            let repository = store.ensure_repository(&root.to_string_lossy())?;
            let outcome = index_repository_bounded(&mut store, &root, None, None)?;
            let skipped = if outcome.skipped_sources > 0 {
                format!(", {} skipped", outcome.skipped_sources)
            } else {
                String::new()
            };
            println!(
                "initialized repository {} at {} ({} sources{})",
                repository.id.0,
                repository.root_path,
                outcome.changed_sources + outcome.unchanged_sources,
                skipped
            );
        }
        Command::Index { path, db, repo } => {
            let mut store = open_store(&db_path(db))?;
            let root = match path {
                Some(path) => scanner::repository_root(&path)?,
                None => match repo {
                    Some(repo) => scanner::repository_root(&repo)?,
                    None => PathBuf::from(select_repository(&store, None)?.root_path),
                },
            };
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
            let _repository = match store.ensure_repository(&root.to_string_lossy()) {
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
        Command::Status { db, repo } => {
            let store = open_store(&db_path(db))?;
            let repository = select_repository(&store, repo.as_deref())?;
            let mut status = serde_json::to_value(store.stats_for_repo(repository.id)?)?;
            let embedder = embedder();
            let (mode, model) = retrieval_mode(embedder.as_deref());
            status["retrieval_mode"] = serde_json::json!(mode);
            if let Some(model) = model {
                status["embedding_model"] = serde_json::json!(model);
            }
            let vector_models: Vec<serde_json::Value> = store
                .vector_models(repository.id)?
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
            repo,
        } => {
            let store = open_store(&db_path(db))?;
            let repository = select_repository(&store, repo.as_deref())?;
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
                repository.id,
                embedder.as_deref(),
                &query_text,
                &QueryOptions {
                    channels,
                    top_n: top,
                    max_tokens: tokens,
                },
            )?;
            if explain {
                eprintln!("{}", serde_json::to_string_pretty(&report.debug)?);
            }
            println!("{}", serde_json::to_string_pretty(&report.packet)?);
        }
        Command::Inspect {
            target,
            value,
            db,
            repo,
        } => {
            let store = open_store(&db_path(db))?;
            let repository = select_repository(&store, repo.as_deref())?;
            match target.as_str() {
                "unit" => {
                    let id: i64 = value.parse().map_err(|_| "unit id must be numeric")?;
                    let unit = store.unit_by_id_in_repo(repository.id, id)?.ok_or_else(
                        || -> Box<dyn std::error::Error + Send + Sync> {
                            format!("unit {id} not found").into()
                        },
                    )?;
                    let anchors = store
                        .anchors_for_unit(id)?
                        .into_iter()
                        .filter_map(|(kind, relationship, anchor_id)| {
                            store
                                .anchor_value(repository.id, &kind, anchor_id)
                                .transpose()
                                .map(|value| {
                                    value.map(|value| {
                                        serde_json::json!({
                                            "kind": kind,
                                            "value": value,
                                            "relationship": relationship,
                                        })
                                    })
                                })
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "unit": unit,
                            "anchors": anchors,
                        }))?
                    );
                }
                "symbol" => {
                    let entries =
                        snoop::mcp::symbol_context_entries(&store, repository.id, &value)?;
                    println!("{}", serde_json::to_string_pretty(&entries)?);
                }
                other => return Err(format!("unknown inspect target: {other}").into()),
            }
        }
        Command::History { symbol, db, repo } => {
            let store = open_store(&db_path(db))?;
            let repository = select_repository(&store, repo.as_deref())?;
            let entries = snoop::mcp::history_entries(&store, repository.id, &symbol)?;
            println!("{}", serde_json::to_string_pretty(&entries)?);
        }
        Command::Sessions { symbol, db, repo } => {
            let store = open_store(&db_path(db))?;
            let repository = select_repository(&store, repo.as_deref())?;

            let mut defining_files = std::collections::HashSet::new();
            for id in store.units_for_anchor(repository.id, "symbol", &symbol, 64)? {
                if let Some(unit) = store.unit_by_id(id)? {
                    if unit.source_kind == snoop::core::SourceKind::Code {
                        for (kind, _relationship, anchor_id) in store.anchors_for_unit(id)? {
                            if kind == "file" {
                                if let Some(file) =
                                    store.anchor_value(repository.id, &kind, anchor_id)?
                                {
                                    defining_files.insert(file);
                                }
                            }
                        }
                    }
                }
            }
            let mut episodes = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for file in &defining_files {
                for id in store.units_for_anchor(repository.id, "file", file, 64)? {
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
        Command::Mcp { db, repo } => {
            let store = open_store(&db_path(db))?;
            let repository = select_repository(&store, repo.as_deref())?;
            let embedder = embedder();
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            snoop::mcp::serve(
                &store,
                repository.id,
                embedder.as_deref(),
                &mut stdin.lock(),
                &mut stdout.lock(),
            )?;
        }
    }
    Ok(())
}
