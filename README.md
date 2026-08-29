# snoop

`snoop` is a local repository context compiler for coding agents.

V1 indexes current code (Rust, Python, TypeScript/TSX), Markdown, text, git history, and prior agent sessions (Pi). It stores canonical atoms and disposable retrieval units in SQLite. It creates deterministic evidence and routing projections. Queries run over four local channels (evidence/routing x BM25/vector) with reciprocal-rank fusion and anchor expansion. It performs no query-time generative-LLM call.

## Build and test

```bash
cargo build
cargo test
```

## CLI

```bash
snoop init .
snoop index
snoop status
snoop query "where is refresh-token validation performed?"
snoop query "where is refresh-token validation performed?" --explain
snoop query "where is refresh-token validation performed?" --evidence-only
snoop inspect unit 381
snoop inspect symbol refresh_session
snoop history refresh_session
snoop sessions refresh_session
```

The default database is `.snoop.db`. Set `SNOOP_DB` or pass `--db` to override it.

Without a configured embedder, Snoop runs in **lexical-and-anchor mode**: evidence and routing
BM25, anchor expansion, role-aware admission, content-hash deduplication, and token budgeting.
No vectors are stored or searched, and `snoop status` reports `"retrieval_mode":
"lexical+anchors"`. Point `SNOOP_EMBED_URL` at a local llama.cpp server to upgrade to hybrid
mode, which adds the evidence-vector and routing-vector channels, four-channel RRF, and
vector near-duplicate filtering (status then reports `"retrieval_mode": "hybrid"` plus

```bash
export SNOOP_EMBED_URL=http://127.0.0.1:8097
export SNOOP_EMBED_VERSION=Qwen3-Embedding-0.6B-Q8_0
```

Query and retrieval are fully deterministic and perform no generative-LLM call at any stage. Packet assembly is role-aware by default:

1. Detect query facets (rationale, evolution, validation, prior work, conflict, current behavior) from the query text.
2. Map source kinds to evidence roles: code -> current truth, docs -> design rationale, git -> change origin, sessions -> prior work.
3. Admit one unit per facet-required role first.
4. Admit one unit per remaining supporting role.
5. Fill the rest of the token budget in fused-rank order.

The query-time Evidence Curator was spiked and deleted after failing its adoption gate at equal token budget (it matched but did not beat the deterministic role-aware builder); the review lives in `.pi-files/review-evidence-curator.md`. The earlier opt-in semantic chunk scorer was retired the same way (zero failures, no recall lift).

## MCP server

`snoop mcp` runs a synchronous stdio MCP server (newline-delimited JSON-RPC 2.0, protocol 2025-06-18) over an existing index:

```bash
snoop mcp --db .snoop.db
```

It exposes exactly three tools:

| Tool | Input | Returns |
|---|---|---|
| `get_repo_context` | `query` (string, required), `max_tokens` (integer, default 6000) | Token-budgeted context packet across current code, docs, git history, and prior agent work |
| `repo_symbol_context` | `symbol` (string, required) | Every unit anchored to the symbol (code, docs, commits) |
| `repo_history` | `symbol` (string, required) | Git commit units that changed the symbol |

Notifications produce no response. Unknown methods return JSON-RPC `-32601` and malformed lines return `-32700`. Tool-usage errors return `-32602` or a tool-level `isError` result. The server never exposes internal channel scores or index internals; responses are finished packets and entries.

## Current scope

Included through V1:

- gitignore-aware repository scanning.
- Rust, Python, and TypeScript/TSX code parsing.
- canonical atoms with offsets, breadcrumbs, and BLAKE3 hashes.
- deterministic retrieval units and routing projections.
- incremental source and unit reuse (git tip boundary, session appends).
- git-history ingestion with diff-to-symbol alignment.
- Pi session adapter normalizing prior agent work into phase-aware, append-stable retrieval segments (deterministic).
- anchor graph and query-time anchor expansion.
- SQLite FTS5 indexing and sqlite-vec cosine distance search (opt-in via `SNOOP_EMBED_URL`).
- local llama.cpp embedding adapter plus a mock adapter for tests.
- evidence-only and four-channel retrieval; lexical-and-anchor mode when no embedder is configured.
- RRF, context budgeting, near-duplicate filtering, role-aware packet assembly, and explanations.
- stdio MCP server with three tools.
