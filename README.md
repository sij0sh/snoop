# Snoop

> Retroactive memory for coding agents.
>
> Snoop turns your repository's existing history into context that an agent can recall when needed.

Your repository already has a memory. It lives in the current code, the commits that shaped it, the documentation that explains it, and earlier agent sessions.

Snoop searches all of that material together.

```text id="dwug3q"
context({
  query: "why does refresh_session rotate the token here?"
})
```

A single question can go beyond the current code and recover how it got there.

## The code is only the latest chapter

Source code is usually the right place to start, but it does not always tell the whole story.

A strange implementation may make sense once you find the commit that introduced it. A design decision may live in Markdown. Another agent may have investigated a bug two weeks ago. An earlier implementation may explain why the obvious approach was abandoned.

Snoop indexes:

* current source code
* Markdown and text
* Git history
* prior Pi agent sessions

A repository question can draw on both its present state and its history.

```text id="50gxpc"
current code  ─────┐
documentation ─────┤
git history ───────┼──> repository memory ──> context
prior sessions ────┘
```

## Memory depends on recall

Creating project knowledge is relatively easy. You can write an architecture document, maintain implementation notes, record decisions, or give agents a memory file.

The harder question comes later:

**Will the agent retrieve the right piece when it matters?**

Useful context can exist in the project and still go unused. The model may not know where to look or realize that an old decision applies to the current problem.

Snoop focuses on recall. The agent asks a question about what it needs to understand:

```text id="gs6omg"
context({
  query: "was this retry behavior intentional?"
})
```

Snoop searches the repository's memory and returns a compact set of relevant evidence. The agent does not need to know which file contains the explanation or which commit matters.

## Retroactive by design

Many memory systems become useful only after installation, once you start feeding them memories. Snoop can also look backward.

An existing repository already contains material such as:

* years of commits
* existing design docs
* current source
* previous agent sessions

Snoop makes that history available without first converting it into a new knowledge format. The repository does not need to have been designed as an agent memory system.

## Recall without flooding the context

More memory is not automatically better. Dumping every related commit, file, note, and old session into the prompt buries the useful evidence.

Snoop returns a bounded context packet. It brings back the evidence that helps answer the current question, while the rest remains available for another query.

Human memory works the same way. Recall is useful because we can retrieve the relevant part without holding everything at once.

## One interface

Coding agents use a single command:

```text id="byy3v9"
context
```

Ask a normal repository question:

```text id="smhsvy"
context({
  query: "where is refresh-token validation performed?"
})
```

Ask about intent:

```text id="s79ssl"
context({
  query: "why was retry ordering changed?"
})
```

Ask what happened before:

```text id="2drsjv"
context({
  query: "has this token reuse bug been investigated before?"
})
```

Or search by name:

```text id="xnfb4d"
context({
  query: "refresh_session"
})
```

Each query asks Snoop to recall useful repository context, so the interface stays the same.

## Local first

Snoop stores repository memory locally in SQLite. It does not use a generative LLM at query time to decide what the repository says.

Lexical retrieval and relationships captured during indexing work out of the box. Local embeddings can add semantic retrieval.

The agent uses the same call either way:

```text id="rflrxn"
context({ query: "..." })
```

## Install

### 1. Install Snoop

macOS and Linux:

```bash id="c5k7z0"
curl -fsSL https://raw.githubusercontent.com/colbymchenry/snoop/main/scripts/install.sh | sh
```

Windows PowerShell:

```powershell id="ntvtck"
irm https://raw.githubusercontent.com/colbymchenry/snoop/main/scripts/install.ps1 | iex
```

Or build from source:

```bash id="0sp10e"
cargo install --path .
```

### 2. Connect your coding agent

```bash id="wcfnho"
snoop install
```

Snoop detects and configures supported agents, including Pi, Claude Code, Cursor, Codex CLI, opencode, Gemini CLI, VS Code with GitHub Copilot, Windsurf, and Kiro.

Preview the configuration without changing anything:

```bash id="wzi3np"
snoop install --list
```

Or configure one integration:

```bash id="hq5y9q"
snoop install --agent <NAME>
```

### 3. Initialize a repository

```bash id="43nemz"
cd your-project
snoop init .
```

Snoop can now recall the repository's existing memory. Continue using your coding agent normally.

## Optional local embeddings

Embeddings are optional. To add semantic vector retrieval, run:

```bash id="gvtzde"
snoop install embedder
```

Snoop installs a local llama.cpp server with `Qwen3-Embedding-0.6B-Q8_0`. This adds semantic retrieval without changing the agent interface.

## Pi and MCP

Pi connects through the bundled Snoop extension. Other supported agents connect through Snoop's MCP server.

Both provide the same command:

```text id="mg1vcl"
context({
  query: "what changed around session refresh and why?"
})
```

Only the integration method differs.

## Configuration

Snoop works with its defaults. You can set these environment variables when needed:

* `SNOOP_DB`: repository database path
* `SNOOP_EMBED_URL`: embedding server
* `SNOOP_EMBED_VERSION`: embedding model identifier
* `SNOOP_ENSURE_TIMEOUT`: refresh budget
* `SNOOP_SESSIONS_ROOT`: Pi session directory

## Why Snoop?

Useful knowledge often already exists in the repository. The difficult part is recalling the right piece at the right time.

Your repository has been accumulating memory since before Snoop was installed. Snoop gives your agent a way to recall it.
