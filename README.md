# snoop

`snoop` is a local repository context compiler for coding agents.

Snoop indexes current code (Rust, Python, TypeScript/TSX, JavaScript/JSX, Go, Java, C#, C, C++), Markdown, text, git history, and prior agent sessions (Pi). It stores self-contained retrieval units in SQLite. It creates deterministic evidence and routing projections. Queries run over four local channels (evidence/routing x BM25/vector) with reciprocal-rank fusion and anchor expansion. It performs no query-time generative-LLM call.

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
snoop ensure .
snoop query "where is refresh-token validation performed?"
snoop query "where is refresh-token validation performed?" --explain
snoop query "where is refresh-token validation performed?" --evidence-only
snoop inspect unit 381
snoop inspect symbol refresh_session
snoop history refresh_session
snoop sessions refresh_session
```

The default database is `~/.snoop/snoop.db`. Set `SNOOP_DB` or pass `--db` to override it.

One database holds exactly one repository. The first `snoop init`/`index`/`ensure` binds the
database to that repository's canonical root; a second root is refused instead of silently
sharing. A database in any other on-disk format is refused too: delete it and index the
repository again.

Queries emit lean packets: each item carries only source kind, locator, evidence text, and timestamp. `snoop query --explain` additionally prints selection diagnostics to stderr (selected unit IDs, source slices, resolved anchors, selection reasons, channel and fused rankings, anchor-expansion decisions). `max_tokens` is an evidence budget: the sum of admitted evidence never exceeds it.

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
## Lifecycle indexing

`snoop ensure` refreshes a repository's index. It is safe to run unattended and is the integration surface for pre-launch freshness:

```bash
snoop ensure [PATH] [--db <DB>] [--timeout <SECS>]
```

- Auto-initializes the repository, then indexes changed sources and embeddings (no cards).
- Prints one JSON object: `"status"` is `refreshed` | `up-to-date` | `timeout` | `locked` | `error` (with `outcome` when refreshed or up-to-date, `error` on failure).
- Exit code 0 for every status except `error` (1): a launch is never blocked. `locked` means another indexer holds the lease and owns freshness.
- `--timeout` defaults to `SNOOP_ENSURE_TIMEOUT` (seconds), else 120. A timed-out run self-heals: git tip stays untouched, deletions are skipped, and the next run completes the refresh.

### Pi extension

`extensions/snoop-pi.ts` spawns `snoop ensure` detached on every pi session start (`startup`, `new`, `resume`, `fork`; not `reload`), so launches stay instant and freshness is eventual. Copy it into place:

```bash
cp extensions/snoop-pi.ts ~/.pi/agent/extensions/   # or <project>/.pi/extensions/
```

- `SNOOP_ENSURE=0` disables the trigger.
- `SNOOP_ENSURE_TIMEOUT` sets the ensure budget in seconds (default 120).
- Spawn failures (missing binary, permissions) are appended to `.snoop-ensure.log` in the project directory instead of failing silently.

### Blocking pre-launch (shell)

When the first query of a session must see a fresh index, block explicitly:

```bash
snoop ensure . --timeout 30 && exec <agent>
```

### Cron

```cron
*/15 * * * * cd /path/to/repo && snoop ensure . >> .snoop-cron.log 2>&1
```

Overlap is safe: a second ensure reports `locked` and exits 0.

### Bounds and tuning

- The deadline plus one in-flight embed batch bounds wall time (ureq caps a batch at 300 s). Blocking callers should size `--timeout` with headroom.
- Concurrency safety is `busy_timeout` plus a per-repository lease: a single active indexer holds within the lease TTL. The TTL is fixed at 360 s (300 s embed-batch cap + 60 s slack) and renewed per embed batch, so a holder that stalls past TTL may be stolen; per-source transactions keep the index consistent. These behaviors are pinned in `tests/concurrency.rs`.
- Tune `SNOOP_ENSURE_TIMEOUT` from `index_runs.duration_ms` (`snoop status`) after real-embedder use; 120 s is provisional.

## MCP server

`snoop mcp` runs a synchronous stdio MCP server (newline-delimited JSON-RPC 2.0, protocol 2025-06-18) over an existing index:

```bash
snoop mcp --db .snoop.db
```

It exposes exactly three tools:

| Tool | Input | Returns |
|---|---|---|
| `get_repo_context` | `query` (string, required), `max_tokens` (integer, default 6000) | Evidence-budgeted context packet across current code, docs, git history, and prior agent work |
| `repo_symbol_context` | `symbol` (string, required) | Every unit anchored to the symbol (code, docs, commits) |
| `repo_history` | `symbol` (string, required) | Git commit units that changed the symbol |

Notifications produce no response. Unknown methods return JSON-RPC `-32601` and malformed lines return `-32700`. Tool-usage errors return `-32602` or a tool-level `isError` result. The server never exposes internal channel scores or index internals; responses are finished packets and entries.

## Current scope

Included:

- gitignore-aware repository scanning.
- Rust, Python, TypeScript/TSX, JavaScript/JSX, Go, Java, C#, C, and C++ code parsing.
- in-memory atom parsing (offsets, breadcrumbs, BLAKE3 hashes) feeding retrieval units.
- deterministic retrieval units and routing projections.
- incremental source and unit reuse (git tip boundary, session appends).
- git-history ingestion with diff-to-symbol alignment.
- Pi session adapter normalizing prior agent work into one unit per user turn (append-stable, deterministic).
- anchor graph and query-time anchor expansion.
- SQLite FTS5 indexing and sqlite-vec cosine distance search (opt-in via `SNOOP_EMBED_URL`).
- local llama.cpp embedding adapter plus a mock adapter for tests.
- evidence-only and four-channel retrieval; lexical-and-anchor mode when no embedder is configured.
- RRF, evidence budgeting, near-duplicate filtering, role-aware packet assembly, and opt-in explain diagnostics.
- stdio MCP server with three tools.
