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
C++, Ruby, PHP, shell, and GDScript code, plus Godot scenes (`.tscn`) and text
resources (`.tres`). Repository scanning respects Git ignore rules.

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
Gemini CLI, VS Code (GitHub Copilot), Windsurf, and Kiro. Every agent except
Pi gets the `snoop mcp` server. Pi gets the extension below, which substitutes
for the MCP server (see "Pi tools as the MCP substitute"). This is the step that connects Snoop to your agent;
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
snoop query "question" --tokens 6000 [--exclude-session ID]
snoop inspect symbol refresh_session [--exclude-session ID]
snoop inspect unit 381
snoop sessions refresh_session
```

`query` limits each source file, commit, or session to three packet items by
default. This diversity cap prevents one source from consuming the context
budget. Packet timestamps use compact relative or calendar dates.
`--exclude-session` is repeatable and removes episodes already visible in the
calling agent's context.

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

## Pi extension

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

### Pi tools as the MCP substitute

The extension substitutes for the `snoop mcp` server. `snoop install` wires Pi
through this extension only, never through MCP. The extension registers the
same two tools, with the same names, parameters, and defaults, and both paths
read the same index:

- `get_repo_context` runs `snoop query <query> --tokens <max_tokens>`.
- `repo_symbol_context` runs `snoop inspect symbol <symbol>`.

Both tools pass the current Pi session ID through `--exclude-session`. This
prevents the current conversation from returning as repository evidence.

The MCP server adds a worker pool and an embed deadline that degrades to
lexical-only retrieval under load. The extension tools run one CLI process per
call and need no server.

## MCP server

Run the stdio MCP server over an existing index:

```bash
snoop mcp
```

It implements JSON-RPC 2.0 and exposes two tools:

- `get_repo_context` accepts `query`, optional `max_tokens`, and optional
  `exclude_sessions` IDs. It returns a token-budgeted context packet.
- `repo_symbol_context` accepts `symbol` and optional `exclude_sessions` IDs.
  It returns code, docs, commits, and agent episodes anchored to that symbol;
  commit and agent-session entries carry human-readable timestamps.

Pi sessions skip this server: the extension above provides the same two tools
through the CLI.
