# Snoop

> Give coding agents compact, evidence-grounded context from a repository's
> code, history, documentation, and prior work.

Coding agents often search the current tree but miss why code changed, where a
symbol is discussed, or what an earlier agent already tried.

## Why Snoop?

Snoop is a local repository context compiler that joins those sources in a
SQLite index. It returns deterministic context packets through the CLI or MCP
without a query-time generative LLM.

- Keep repository evidence local in SQLite.
- Combine current code, Markdown, text, up to 500 Git commits, and Pi session history.
- Retrieve with lexical search and symbol anchors without configuring an
  embedding model.
- Add local llama.cpp embeddings for hybrid lexical and vector retrieval.
- Cap each result by an explicit evidence token budget.

Snoop parses Rust, Python, TypeScript, TSX, JavaScript, JSX, Go, Java, C#, C,
C++, Ruby, PHP, and shell code. Repository scanning respects Git ignore rules.

## Prerequisites

Nothing for the CLI itself. Git is needed only when indexing Git history. Snoop
installs the optional embedder itself (see "Optional embeddings").

## Install

### 1. Install the CLI

No Rust toolchain required — one command grabs the right build for your OS:

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/colbymchenry/snoop/main/scripts/install.sh | sh
```

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/colbymchenry/snoop/main/scripts/install.ps1 | iex
```

Building from source also works: `cargo install --path .` from a checkout.

### 2. Wire up your agent(s)

In a new terminal, run the installer to connect Snoop to the agents you use:

```bash
snoop install
```

Detects and auto-configures Pi, Claude Code, Cursor, Codex CLI, opencode,
Gemini CLI, VS Code (GitHub Copilot), Windsurf, and Kiro — wiring the `snoop
mcp` server into each. This is the step that connects Snoop to your agent;
installing the CLI in step 1 does not do it on its own. It only wires up your
agent — it does not index any code; building each project's index is the
separate `snoop init` in step 3.

`snoop install --list` previews detection without writing anything. `snoop
install --agent <NAME>` wires one agent even when detection misses it.

### 3. Initialize each project

```bash
cd your-project
snoop init .
snoop query "where is refresh-token validation performed?"
```

The query prints a JSON context packet. Add `--explain` to write selection
diagnostics to stderr. Use `--evidence-only` to omit routing channels.

Each database belongs to exactly one canonical repository root. Use a separate
database for each repository. The default path is `<repository
root>/.agents/snoop.db`. `--db <PATH>` overrides both that default and
`SNOOP_DB`.

## Optional embeddings

Without an embedder, Snoop uses BM25 retrieval, anchor expansion, role-aware
admission, deduplication, and token budgeting. `snoop status` reports a
`"retrieval_mode"` of `"lexical+anchors"`.

To add vector channels, install a local llama.cpp embedding server once:

```bash
snoop install embedder
```

That downloads the llama.cpp server and the Qwen3-Embedding-0.6B-Q8_0 model
into `~/.snoop` and writes `~/.snoop/config.json`. Start the server, then
build vectors in each indexed project:

```bash
snoop embed
snoop index
```

Hybrid mode combines BM25 and vector results with reciprocal-rank fusion.
`snoop status` then reports a `"retrieval_mode"` of `"hybrid"`. `SNOOP_EMBED_URL`
and `SNOOP_EMBED_VERSION` still override `config.json` for a manually managed
server.

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

`ensure` prints one JSON object with a `refreshed`, `up-to-date`, `timeout`,
`locked`, or `error` status. A concurrent process reports `locked`. Every status
except `error` exits successfully, so freshness work does not block an agent
launch.

## Configuration

- `SNOOP_DB`: Database path when `--db` is absent. Unset by default; without
  it, the database path defaults to `<repository root>/.agents/snoop.db`.
- `SNOOP_EMBED_URL`: llama.cpp server that enables hybrid retrieval. Unset by
  default; Snoop then reads `~/.snoop/config.json` written by `snoop install
  embedder`.
- `SNOOP_EMBED_VERSION`: Embedding model identifier stored with vectors.
  Defaults to `Qwen3-Embedding-0.6B-Q8_0`.
- `SNOOP_ENSURE_TIMEOUT`: Extension refresh budget in seconds. Defaults to
  `120`.
- `SNOOP_SESSIONS_ROOT`: Pi session directory. Defaults to
  `~/.pi/agent/sessions`.

## Pi session refresh

[`extensions/snoop-pi.ts`](extensions/snoop-pi.ts) starts a detached
`snoop ensure` on Pi session startup, new, resume, and fork events. `snoop
install` copies it to `~/.pi/agent/extensions/`. To install it manually, copy
it to the user or project extension directory:

```bash
cp extensions/snoop-pi.ts ~/.pi/agent/extensions/
# or: cp extensions/snoop-pi.ts .pi/extensions/
```

Set `SNOOP_ENSURE=0` to disable the trigger. Spawn failures are appended to
`.snoop-ensure.log` in the project directory.

The extension also registers two Pi tools backed by the CLI, mirroring the MCP
tools below for sessions without the MCP server: `get_repo_context` runs
`snoop query`, and `repo_symbol_context` runs `snoop inspect symbol`.

## MCP server

Run the stdio MCP server over an existing index:

```bash
snoop mcp
```

It implements JSON-RPC 2.0 and exposes two tools:

- `get_repo_context` accepts `query` and optional `max_tokens`. It returns a
  token-budgeted context packet.
- `repo_symbol_context` accepts `symbol`. It returns code, docs, commits, and
  agent episodes anchored to that symbol; commit entries carry their timestamp
  and evidence text.
