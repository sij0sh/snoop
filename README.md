# Snoop

> Give coding agents compact, evidence-grounded context from a repository's code, history, documentation, and prior work.

Snoop is a local repository context compiler that builds a SQLite index and returns deterministic context packets without a query-time generative LLM.

## Why Snoop?

Coding agents often search the current tree but miss why code changed, where a symbol is discussed, or what an earlier agent already tried. Snoop joins those sources into retrieval units that an agent can query through the CLI or MCP.

- Keep repository evidence local in SQLite.
- Combine current code, Markdown, text, up to 500 Git commits, and Pi session history.
- Retrieve with lexical search and symbol anchors without configuring an embedding model.
- Add local llama.cpp embeddings for hybrid lexical and vector retrieval.
- Cap each result by an explicit evidence token budget.

Snoop parses Rust, Python, TypeScript, TSX, JavaScript, JSX, Go, Java, C#, C, C++, Ruby, PHP, and shell code. Repository scanning respects Git ignore rules.

## Prerequisites

- A Rust toolchain with Cargo.
- Git when indexing Git history.
- An optional llama.cpp embedding server for hybrid retrieval.

## Install

From the repository root:

```bash
cargo install --path .
```

For development, run `cargo build` and `cargo test`.

## Quick start

Create one database for the repository, index it, and ask a question:

```bash
cd /path/to/repository
export SNOOP_DB="$PWD/.snoop.db"
snoop init .
snoop status
snoop query "where is refresh-token validation performed?"
```

The query prints a JSON context packet. Add `--explain` to write selection diagnostics to stderr, or use `--evidence-only` to omit routing channels.

Each database belongs to exactly one canonical repository root. Use a separate database for each repository. The default path is `~/.snoop/snoop.db`; `--db <PATH>` overrides both that default and `SNOOP_DB`.

## Retrieval modes

Without an embedder, Snoop uses BM25 retrieval, anchor expansion, role-aware admission, deduplication, and token budgeting. `snoop status` reports `"retrieval_mode": "lexical+anchors"`.

To add vector channels, point Snoop at a compatible local llama.cpp server:

```bash
export SNOOP_EMBED_URL=http://127.0.0.1:8097
export SNOOP_EMBED_VERSION=Qwen3-Embedding-0.6B-Q8_0
snoop index
```

Hybrid mode combines evidence and routing results from BM25 and vector search with reciprocal-rank fusion. `snoop status` then reports `"retrieval_mode": "hybrid"`.

## Common commands

```bash
snoop index [PATH]                  # Refresh an existing index
snoop ensure [PATH] --timeout 120   # Refresh safely for unattended use
snoop query "question" --tokens 6000
snoop inspect symbol refresh_session
snoop inspect unit 381
snoop history refresh_session
snoop sessions refresh_session
```

`ensure` prints one JSON object with a `refreshed`, `up-to-date`, `timeout`, `locked`, or `error` status. Concurrent refreshes are safe: a second process reports `locked`. Every status except `error` exits successfully, so freshness work does not block an agent launch.

## Configuration

| Setting | Purpose | Default |
|---|---|---|
| `SNOOP_DB` | Database path when `--db` is absent | `~/.snoop/snoop.db` |
| `SNOOP_EMBED_URL` | Enable hybrid retrieval through a llama.cpp server | Unset |
| `SNOOP_EMBED_VERSION` | Identify the embedding model stored with vectors | `Qwen3-Embedding-0.6B-Q8_0` |
| `SNOOP_ENSURE_TIMEOUT` | Budget for extension-triggered refreshes | `120` seconds |
| `SNOOP_SESSIONS_ROOT` | Override the Pi session directory | `~/.pi/agent/sessions` |

## Pi session refresh

[`extensions/snoop-pi.ts`](extensions/snoop-pi.ts) starts a detached `snoop ensure` on Pi session startup, new, resume, and fork events. Install it in the user or project extension directory:

```bash
cp extensions/snoop-pi.ts ~/.pi/agent/extensions/
# or: cp extensions/snoop-pi.ts .pi/extensions/
```

Set `SNOOP_ENSURE=0` to disable the trigger. Spawn failures are appended to `.snoop-ensure.log` in the project directory.

## MCP server

Run the stdio MCP server over an existing index:

```bash
snoop mcp --db .snoop.db
```

It implements JSON-RPC 2.0 and exposes three tools:

| Tool | Input | Result |
|---|---|---|
| `get_repo_context` | `query`, optional `max_tokens` | A token-budgeted context packet |
| `repo_symbol_context` | `symbol` | Code, docs, commits, and agent episodes anchored to a symbol |
| `repo_history` | `symbol` | Git commit units that changed a symbol |
