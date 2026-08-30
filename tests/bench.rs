use std::path::Path;
use std::process::Command;
use std::time::Instant;

use snoop::core::RepoId;
use snoop::inference::MockEmbedder;
use snoop::ingest::index_repository_bounded;
use snoop::runtime::{query, QueryChannels, QueryOptions};
use snoop::store::Store;

fn env_embedder() -> Box<dyn snoop::inference::Embedder> {
    let url = std::env::var("SNOOP_EMBED_URL").unwrap_or_else(|_| "mock".to_string());
    if url == "mock" {
        Box::new(MockEmbedder::new("mock-v1"))
    } else {
        let version = std::env::var("SNOOP_EMBED_VERSION")
            .unwrap_or_else(|_| "Qwen3-Embedding-0.6B-Q8_0".to_string());
        Box::new(snoop::inference::LlamaServerEmbedder::new(&url, &version))
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "bench")
        .env("GIT_AUTHOR_EMAIL", "bench@example.com")
        .env("GIT_COMMITTER_NAME", "bench")
        .env("GIT_COMMITTER_EMAIL", "bench@example.com")
        .env("GIT_AUTHOR_DATE", "2026-08-20T12:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-08-20T12:00:00Z")
        .status()
        .expect("git spawns");
    assert!(status.success(), "git {args:?} failed");
}

const SESSION_1_LINES: &[&str] = &[
    r#"{"type":"session","version":3,"id":"bench-session-1","timestamp":"2026-08-21T09:00:00.000Z","cwd":"/tmp/bench"}"#,
    r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2026-08-21T09:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Investigate why token rotation in refresh_session reuses stale tokens"}]}}"#,
    r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-08-21T09:01:30.000Z","message":{"role":"assistant","content":[{"type":"text","text":"The rotation ordering was wrong: rotate was called before validation. Fixed by validating first."},{"type":"toolCall","id":"c1","name":"edit","arguments":{"path":"src/auth.rs"}}]}}"#,
    r#"{"type":"message","id":"u2","parentId":"a1","timestamp":"2026-08-21T09:05:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Run the auth retry tests"}]}}"#,
    r#"{"type":"message","id":"a2","parentId":"u2","timestamp":"2026-08-21T09:05:02.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"c2","name":"bash","arguments":{"command":"cargo test auth"}}]}}"#,
];

const SESSION_2_LINES: &[&str] = &[
    r#"{"type":"session","version":3,"id":"bench-session-2","timestamp":"2026-08-21T11:00:00.000Z","cwd":"/tmp/bench"}"#,
    r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2026-08-21T11:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Refresh tokens still leak under concurrent load despite the validation reorder"}]}}"#,
    r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-08-21T11:01:30.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Mutex first is the real fix; validation ordering does not stop the leak."},{"type":"toolCall","id":"c1","name":"edit","arguments":{"path":"src/auth.rs"}}]}}"#,
];

const SESSION_3_LINES: &[&str] = &[
    r#"{"type":"session","version":3,"id":"bench-session-3","timestamp":"2026-08-22T10:00:00.000Z","cwd":"/tmp/bench"}"#,
    r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2026-08-22T10:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Run the full auth test suite and report the results"}]}}"#,
    r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-08-22T10:01:02.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"cargo test auth -- --ignored"}}]}}"#,
];

fn write_session(sessions_root: &Path, canonical_root: &Path, name: &str, lines: &[&str]) {
    let directory = sessions_root.join(snoop::ingest::harness::session_directory_name(
        &canonical_root.to_string_lossy(),
    ));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join(name), lines.join("\n") + "\n").unwrap();
}

fn build_fixture(root: &Path, sessions_root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(root.join("distractors")).unwrap();

    // c1: v1 code, buggy ordering.
    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session(token: Token) {\n    rotate_token(token);\n    validate_token(token);\n}\n\nfn rotate_token(t: Token) {}\nfn validate_token(t: Token) {}\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "# Bench\n\nAuth module.\n").unwrap();
    git(root, &["init", "--quiet"]);
    git(root, &["add", "."]);
    git(
        root,
        &["commit", "--quiet", "-m", "introduce session refresh"],
    );

    // c2: rename.
    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session(token: Token) {\n    rotate_token(token);\n    validate_refresh_token(token);\n}\n\nfn rotate_token(t: Token) {}\nfn validate_refresh_token(t: Token) {}\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &[
            "commit",
            "--quiet",
            "-m",
            "rename validate_token to validate_refresh_token",
        ],
    );

    // c3: fix ordering, state the invariant.
    std::fs::write(
        root.join("src/auth.rs"),
        "pub fn refresh_session(token: Token) {\n    // tokens are single-use: validate before rotation\n    validate_refresh_token(token);\n    rotate_token(token);\n}\n\nfn validate_refresh_token(t: Token) {}\nfn rotate_token(t: Token) {}\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &[
            "commit",
            "--quiet",
            "-m",
            "prevent stale refresh-token reuse via rotation",
        ],
    );

    // c4: retry module.
    let mut retry_source = String::from("pub fn retry_with_backoff() {\n");
    for index in 0..120 {
        retry_source.push_str(&format!(
            "    let attempt{index} = backoff() + {index};\n    let window{index} = attempt{index} * 2;\n"
        ));
    }
    retry_source
        .push_str("    wait(backoff());\n}\n\nfn backoff() -> u32 { 8 }\nfn wait(d: u32) {}\n");
    std::fs::write(root.join("src/retry.rs"), retry_source).unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &[
            "commit",
            "--quiet",
            "-m",
            "add retry with exponential backoff",
        ],
    );

    // c5: docs, second invariant site, distractors.
    std::fs::write(
        root.join("docs/design.md"),
        "# Token rotation\n\n## Decision\n\n`refresh_session` validates the token before rotating because stale tokens must never be reused.\n\n## Retry\n\n`retry_with_backoff` uses exponential backoff to avoid thundering herds.\n\n## Backpressure\n\nThe auth layer must drop refreshes when the queue is saturated instead of retrying in a flood.\n\n## Legacy v1 flow\n\nThe legacy v1 flow called `rotate_token` before validating the token; that ordering was retired.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/auth_v2.rs"),
        "// tokens are single-use: the v2 path enforces the same invariant\npub fn refresh_v2(token: Token) {\n    validate_refresh_token(token);\n    rotate_token(token);\n}\n\nfn validate_refresh_token(t: Token) {}\nfn rotate_token(t: Token) {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("distractors/legacy_queue.rs"),
        "// retired prototype\npub fn legacy_enqueue(req: Request) -> usize {\n    // queue retry flood simulator: retry in a tight loop on any queue error\n    let mut flood = 0;\n    loop {\n        flood += 1;\n        if legacy_queue_ok() { break flood; }\n    }\n}\nfn legacy_queue_ok() -> bool { false }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("distractors/legacy_backoff.rs"),
        "// retired prototype\nfn legacy_backoff(attempt: u32) -> u64 {\n    // retry backoff delay: fixed delay, not exponential\n    42\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("distractors/legacy_retry.md"),
        "# Legacy retry notes\n\nThe retired retry flood controller used a queue with fixed backoff delay before the flood dropped refresh requests.\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &["commit", "--quiet", "-m", "document decisions, add v2 path"],
    );

    let canonical = root.canonicalize().unwrap();
    write_session(
        sessions_root,
        &canonical,
        "2026-08-21T09-00-00-000Z_bench-session-1.jsonl",
        SESSION_1_LINES,
    );
    write_session(
        sessions_root,
        &canonical,
        "2026-08-21T11-00-00-000Z_bench-session-2.jsonl",
        SESSION_2_LINES,
    );
    write_session(
        sessions_root,
        &canonical,
        "2026-08-22T10-00-00-000Z_bench-session-3.jsonl",
        SESSION_3_LINES,
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    CurrentCode,
    Rationale,
    Evolution,
    PriorAttempt,
    InvokedVsPassed,
    RenamedSymbol,
    StaleDocs,
    MultiFileInvariant,
    ContradictorySessions,
    DistractorHeavy,
}

impl Class {
    fn as_str(self) -> &'static str {
        match self {
            Self::CurrentCode => "current-code",
            Self::Rationale => "rationale",
            Self::Evolution => "evolution",
            Self::PriorAttempt => "prior-attempt",
            Self::InvokedVsPassed => "invoked-vs-passed",
            Self::RenamedSymbol => "renamed-symbol",
            Self::StaleDocs => "stale-docs",
            Self::MultiFileInvariant => "multi-file-invariant",
            Self::ContradictorySessions => "contradictory-sessions",
            Self::DistractorHeavy => "distractor-heavy",
        }
    }
}

const CLASSES: &[Class] = &[
    Class::CurrentCode,
    Class::Rationale,
    Class::Evolution,
    Class::PriorAttempt,
    Class::InvokedVsPassed,
    Class::RenamedSymbol,
    Class::StaleDocs,
    Class::MultiFileInvariant,
    Class::ContradictorySessions,
    Class::DistractorHeavy,
];

/// Answer key: a needle must appear verbatim in the unit's locator or
/// evidence text. Keys resolve to unit ids once after indexing; scoring is
/// id-based, never substring-based.
#[derive(Clone, Copy)]
enum GoldKey {
    Any(&'static str),
}

struct BenchQuestion {
    class: Class,
    query: &'static str,
    gold: &'static [GoldKey],
}

const QUESTIONS: &[BenchQuestion] = &[
    BenchQuestion {
        class: Class::CurrentCode,
        query: "how does token refresh validate before rotation",
        gold: &[GoldKey::Any("validate_refresh_token")],
    },
    BenchQuestion {
        class: Class::CurrentCode,
        query: "retry policy backoff delay",
        gold: &[GoldKey::Any("retry_with_backoff")],
    },
    BenchQuestion {
        class: Class::Rationale,
        query: "why does refresh_session validate before rotating the token",
        gold: &[GoldKey::Any("stale tokens must never be reused")],
    },
    BenchQuestion {
        class: Class::Rationale,
        query: "why exponential backoff for retries",
        gold: &[GoldKey::Any("thundering herds")],
    },
    BenchQuestion {
        class: Class::Evolution,
        query: "when was stale refresh-token reuse prevented",
        gold: &[GoldKey::Any("prevent stale refresh-token reuse")],
    },
    BenchQuestion {
        class: Class::Evolution,
        query: "when was retry with backoff introduced",
        gold: &[GoldKey::Any("add retry with exponential backoff")],
    },
    BenchQuestion {
        class: Class::PriorAttempt,
        query: "has an agent investigated token rotation reuse before",
        gold: &[GoldKey::Any("rotation ordering was wrong")],
    },
    BenchQuestion {
        class: Class::InvokedVsPassed,
        query: "did the auth tests pass or were they only invoked",
        gold: &[GoldKey::Any("pi-session:bench-session-3")],
    },
    BenchQuestion {
        class: Class::RenamedSymbol,
        query: "was validate_token renamed",
        gold: &[GoldKey::Any("rename validate_token")],
    },
    BenchQuestion {
        class: Class::StaleDocs,
        query: "what did the legacy v1 refresh flow do",
        gold: &[GoldKey::Any("Legacy v1 flow")],
    },
    BenchQuestion {
        class: Class::MultiFileInvariant,
        query: "single-use token invariant across auth and v2 components",
        gold: &[GoldKey::Any("tokens are single-use")],
    },
    BenchQuestion {
        class: Class::ContradictorySessions,
        query: "was the fix validation ordering or the mutex",
        gold: &[
            GoldKey::Any("rotation ordering was wrong"),
            GoldKey::Any("Mutex first is the real fix"),
        ],
    },
    BenchQuestion {
        class: Class::DistractorHeavy,
        query: "queue backpressure retry flood behavior",
        gold: &[GoldKey::Any("drop refreshes when the queue is saturated")],
    },
];

struct Config {
    name: &'static str,
    channels: QueryChannels,
}

fn configs() -> Vec<Config> {
    vec![
        Config {
            name: "A: code BM25 only",
            channels: QueryChannels::evidence_lexical_only(),
        },
        Config {
            name: "B: A + code vectors",
            channels: QueryChannels::evidence_only(),
        },
        Config {
            name: "C: four sources, single representation",
            channels: QueryChannels::evidence_only(),
        },
        Config {
            name: "D: four sources, dual representation",
            channels: QueryChannels::for_embedder(Some(&MockEmbedder::new("mock-v1"))),
        },
        Config {
            name: "E: D + role-aware admission",
            channels: QueryChannels::for_embedder(Some(&MockEmbedder::new("mock-v1"))),
        },
    ]
}

#[derive(Default)]
struct QuestionResult {
    recall: f64,
    precision: f64,
    density: f64,
    tokens: usize,
    latency_ms: f64,
}

#[derive(Default)]
struct ConfigResult {
    questions: Vec<QuestionResult>,
    by_class: Vec<(&'static str, f64)>,
    misses: Vec<(usize, &'static str)>,
}

fn is_distractor(locator: &str) -> bool {
    locator.starts_with("distractors/")
}

/// Resolve every gold key to the set of unit ids whose locator or evidence
/// contains the needle. Panics when a key matches nothing: the answer key is
/// part of the fixture contract.
fn resolve_gold(store: &Store, repo: RepoId, question: &BenchQuestion) -> Vec<Vec<i64>> {
    question
        .gold
        .iter()
        .map(|key| {
            let GoldKey::Any(needle) = key;
            let matched: Vec<i64> = store
                .unit_ids(repo)
                .unwrap()
                .into_iter()
                .filter(|id| {
                    store.unit_by_id(*id).unwrap().is_some_and(|unit| {
                        unit.locator.contains(needle) || unit.evidence_text.contains(needle)
                    })
                })
                .collect();
            assert!(
                !matched.is_empty(),
                "gold key {needle:?} matched no unit; fixture drifted"
            );
            matched
        })
        .collect()
}

fn classify_miss(
    question_index: usize,
    gold_flat: &[i64],
    store: &Store,
    repo: RepoId,
    options: &QueryOptions,
    report: &snoop::runtime::QueryReport,
) -> &'static str {
    let query = &QUESTIONS[question_index].query;
    let lexical: Vec<i64> = store
        .fts_search(repo, "evidence_text", query, options.top_n)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let gold_in_lexical = gold_flat.iter().any(|id| lexical.contains(id));
    let gold_in_fused = report
        .debug
        .as_ref()
        .expect("bench queries run with diagnostics")
        .fused
        .iter()
        .any(|(id, _, _)| gold_flat.contains(id));
    if gold_in_fused {
        return "NEAR_DUP_OR_BUDGET_DROP";
    }
    if gold_in_lexical {
        return "FUSION_DROP";
    }
    "CHANNEL_MISS"
}

fn run_config(
    config: &Config,
    store: &Store,
    repo: RepoId,
    embedder: &dyn snoop::inference::Embedder,
    answer_key: &[Vec<Vec<i64>>],
) -> ConfigResult {
    let options = QueryOptions {
        channels: config.channels,
        top_n: 10,
        max_tokens: 2_000,
        // Scoring keys on unit ids and fused ranks, which live in diagnostics.
        diagnostics: true,
    };
    let mut result = ConfigResult::default();
    for class in CLASSES {
        result.by_class.push((class.as_str(), 0.0));
    }
    for (index, question) in QUESTIONS.iter().enumerate() {
        let gold = &answer_key[index];
        let start = Instant::now();
        let report = query(store, repo, Some(embedder), question.query, &options).unwrap();
        let elapsed = start.elapsed().as_secs_f64() * 1_000.0;
        let diagnostics = report.debug.as_ref().expect("bench diagnostics");
        let packet_ids: Vec<i64> = diagnostics
            .items
            .iter()
            .map(|item| item.unit_id.0)
            .collect();
        let gold_flat: Vec<i64> = gold.iter().flatten().copied().collect();
        let recall = gold
            .iter()
            .all(|key_units| key_units.iter().any(|id| packet_ids.contains(id)));
        let distractor_items = report
            .packet
            .items
            .iter()
            .filter(|item| is_distractor(&item.source_locator))
            .count();
        let precision = 1.0 - (distractor_items as f64 / packet_ids.len().max(1) as f64);
        let gold_tokens: usize = diagnostics
            .items
            .iter()
            .filter(|item| gold_flat.contains(&item.unit_id.0))
            .map(|item| {
                store
                    .unit_by_id(item.unit_id.0)
                    .unwrap()
                    .map(|unit| unit.token_count)
                    .unwrap_or(0)
            })
            .sum();
        let density = gold_tokens as f64 / report.packet.token_count.max(1) as f64;
        let miss =
            (!recall).then(|| classify_miss(index, &gold_flat, store, repo, &options, &report));
        if let Some(miss) = miss {
            result.misses.push((index, miss));
        } else if let Some((_, value)) = result
            .by_class
            .iter_mut()
            .find(|(name, _)| *name == question.class.as_str())
        {
            *value += 1.0;
        }
        result.questions.push(QuestionResult {
            recall: if recall { 1.0 } else { 0.0 },
            precision,
            density,
            tokens: report.packet.token_count,
            latency_ms: elapsed,
        });
    }
    for class in CLASSES {
        let count = QUESTIONS.iter().filter(|q| q.class == *class).count();
        if count == 0 {
            continue;
        }
        if let Some((_, value)) = result
            .by_class
            .iter_mut()
            .find(|(name, _)| *name == class.as_str())
        {
            *value /= count as f64;
        }
    }
    result
}

fn incremental_cost(root: &Path, db: &Path) -> f64 {
    let mut store = Store::open(db).unwrap();
    let embedder = env_embedder();
    index_repository_bounded(&mut store, root, Some(embedder.as_ref()), None).unwrap();
    let content = std::fs::read_to_string(root.join("src/retry.rs")).unwrap();
    std::fs::write(root.join("src/retry.rs"), format!("{content}\n// touch\n")).unwrap();
    let start = Instant::now();
    index_repository_bounded(&mut store, root, Some(embedder.as_ref()), None).unwrap();
    std::fs::write(root.join("src/retry.rs"), content).unwrap();
    index_repository_bounded(&mut store, root, Some(embedder.as_ref()), None).unwrap();
    start.elapsed().as_secs_f64() * 1_000.0
}

#[test]
#[ignore]
fn benchmark_config_table() {
    let directory = tempfile::tempdir().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    build_fixture(directory.path(), sessions_root.path());
    std::env::set_var("SNOOP_SESSIONS_ROOT", sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    let embedder = env_embedder();
    let outcome =
        index_repository_bounded(&mut store, directory.path(), Some(embedder.as_ref()), None)
            .unwrap();

    let stats = store.stats_for_repo(outcome.repo_id).unwrap();
    println!(
        "fixture: {} sources, {} units, {} vectors",
        stats.sources, stats.units, stats.vectors
    );

    // Answer key: resolve gold needles to unit ids once, then score by id.
    // Each question's key resolves to one set of unit ids per needle; a
    // question is answered when every needle has at least one of its units
    // in the packet.
    let answer_key: Vec<Vec<Vec<i64>>> = QUESTIONS
        .iter()
        .map(|question| resolve_gold(&store, outcome.repo_id, question))
        .collect();
    for (index, keys) in answer_key.iter().enumerate() {
        println!(
            "answer q{} [{}] -> {:?}",
            index + 1,
            QUESTIONS[index].class.as_str(),
            keys.iter().map(Vec::as_slice).collect::<Vec<_>>()
        );
    }

    let configs = configs();

    let mut table =
        String::from("| arm | recall | precision | density | avg tokens | avg ms | misses |\n");
    table.push_str("|---|---|---|---|---|---|---|\n");
    for config in &configs {
        let result = run_config(
            config,
            &store,
            outcome.repo_id,
            embedder.as_ref(),
            &answer_key,
        );
        let recall: f64 =
            result.questions.iter().map(|q| q.recall).sum::<f64>() / result.questions.len() as f64;
        let precision: f64 = result.questions.iter().map(|q| q.precision).sum::<f64>()
            / result.questions.len() as f64;
        let density: f64 =
            result.questions.iter().map(|q| q.density).sum::<f64>() / result.questions.len() as f64;
        let avg_tokens: usize =
            result.questions.iter().map(|q| q.tokens).sum::<usize>() / result.questions.len();
        let avg_latency: f64 = result.questions.iter().map(|q| q.latency_ms).sum::<f64>()
            / result.questions.len() as f64;
        let taxonomy = if result.misses.is_empty() {
            "-".to_string()
        } else {
            result
                .misses
                .iter()
                .map(|(index, class)| format!("q{}:{}", index + 1, class))
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "| {} | {:.3} | {:.3} | {:.3} | {} | {:.1}ms | {} |",
            config.name, recall, precision, density, avg_tokens, avg_latency, taxonomy
        );
        table.push_str(&format!(
            "| {} | {:.3} | {:.3} | {:.3} | {} | {:.1} | {} |\n",
            config.name, recall, precision, density, avg_tokens, avg_latency, taxonomy
        ));
        let classes = result
            .by_class
            .iter()
            .map(|(name, value)| format!("{name} {:.2}", value))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  by class: {}", classes);
    }

    let inc_ms = incremental_cost(directory.path(), &directory.path().join("bench.db"));
    println!(
        "incremental-index cost (single-file edit, reindex): {:.1}ms",
        inc_ms
    );

    std::env::remove_var("SNOOP_SESSIONS_ROOT");
}
