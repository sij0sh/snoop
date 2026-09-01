# Snoop `get_repo_context`: Sample Analysis, Gaps, and Improvement Plan

Date: 2026-09-01
Scope: real `get_repo_context` usage captured from Pi session history, plus live
reproductions against existing indexes. Companion tool:
`.agents/artifacts/analyze_grc.py` (re-runnable scanner).

## 1. Method and Samples

Scanned `~/.pi/agent/sessions/**` (mtime within 45 days) for `get_repo_context`
tool calls paired with their results. Snoop is newly installed, so the full
population is small and was captured exhaustively:

| Sample | Repo | Items | Tokens | Composition |
|---|---|---|---|---|
| C1 2026-09-01 12:28 | choreograph | 29 | 11960/12000 | 13 commit, 6 session, 6 code, 4 markdown |
| C2 2026-09-01 12:31 | choreograph | 25 | 9981/10000 | 19 commit (15 from one commit), 6 session |
| C3 2026-08-31 21:12 | RoastMyHarness | 19 | 5956/6000 | 7 commit, 4 session, code, markdown |
| C4 2026-08-31 21:57 | RoastMyHarness | 12 | 5975/6000 | 8 commit (8 from one commit), 2 session, 1 markdown |
| C5 2026-08-31 22:23 | RoastMyHarness | 5 | 991/1000 | 3 commit, 1 code, 1 session (self) |
| C6 2026-08-31 20:04 | cheat-codes | - | - | error: "index a repository first" |

Totals: 90 items across 5 packets. 60% GitCommit, 24% AgentSession, 12% Code,
3% Markdown. 90/90 items carry raw epoch timestamps.

Live reproductions (queries replayed against the current indexes):

- Choreograph C2 query: 23 items, 13 from commit `300a8479`; the calling
  session's own episode consumed 23% of packet characters; two prior sessions
  consumed another 16%.
- Snoop repo query: `src/runtime/tests.rs` appeared at ranks 1, 4, 7, 8, 10 of
  the packet (5 units from one file).

## 2. What Works Well (verified)

- Multi-source join: every packet blends at least two source kinds; commits,
  sessions, code, and docs all appear. Session episodes can carry real value
  (e.g. a prior scope/analysis episode with decisions and unknowns ranked #2).
- Budget adherence: 5/5 packets respected the token budget exactly.
- Role-aware admission floor: queries with history/why/prior-work facets get a
  commit, session, code, or doc item front-loaded per required role.
- Errors surface as tool errors, not silent empty packets (C6).
- Locators are stable and grep-able (`pi-session:<uuid>`, `git:<sha>`,
  file paths).

## 3. Gaps (each verified in samples)

### G1. The calling session returns as evidence (self-hit)

- 2/5 packets contained items from the session that made the call; best ranks
  8 and 3. In the live repro the calling session consumed 23% of packet chars.
- Mechanism: `snoop ensure` indexes the current session on start/resume; agents
  also re-index mid-session (`snoop index` / `ensure` in bash). The query text
  is usually derived from the current user prompt, so the episode containing
  that prompt is a near-perfect BM25/vector match. The agent gets back text it
  already has in its context window. This is pure waste and, at best ranks,
  crowds out real prior work.
- Extension design gap: `get_repo_context` passes only `query` and
  `max_tokens`; the CLI has no way to know which session is calling.

### G2. User-prompt echoes from prior sessions rank at the top

- C1 rank #1 was a *prior* session's episode whose body begins with the user
  prompt line plus a raw tool-call transcript ("User: Continue workflow...
  Tool: bash pwd && ..."). Similar echo pattern in C2 rank #8.
- Episodes are indexed as full turn transcripts, so prompts (high token overlap
  with queries) outrank the informative middle content. Not fixed by G1's
  filter because the session is different.

### G3. Per-source flooding: one commit or file fills the packet

- 4/5 packets had >=3 items from a single locator; worst case 15 items from one
  commit (60% of items), 8/12 in C4, 7 from one commit in C3, and 5 units of
  one code file in a live repro.
- Mechanism: commits are emitted as many small per-file/per-symbol hunk units
  (`src/ingest/git/emit.rs`); the same hunk body under different symbol
  breadcrumbs gets a different `content_hash`, so hash dedup misses it. The
  vector near-dup gate (0.985 cosine) applies only in hybrid mode and only
  within the same role bucket; this install is lexical (`retrieval_mode:
  lexical+anchors`), so no near-dup control runs at all. The runtime lacks a
  per-source admission cap, and near-identical hunks consume `top_n = 25` pool
  slots that crowd out other sources.

### G4. Timestamps are raw epoch seconds

- 90/90 packet items carry `"timestamp": 1788109899`. The consumer is an LLM;
  converting a 10-digit epoch to "how old is this" on every item is cognitive
  overhead with zero benefit. The stated purpose is only a sense of age.
- Inconsistency: `repo_symbol_context` attaches `timestamp` to GitCommit
  entries only; AgentSession episodes (which have timestamps in metadata) get
  none. `snoop sessions` and `snoop inspect unit` also emit raw epoch.

### G5. Packet order is admission order, not relevance order

- Items render in role-admission sequence (required roles, supporting roles,
  then rank-ordered fill). Near-identical commit hunks from the same commit
  appear scattered (ranks 1, 3, 4, 6, 7...), forcing the reader to reassemble
  context. Minor once G3 is fixed, but ordering is still not relevance-first.

### G6. Unindexed repo produces a bare error

- C6 returned `error: index a repository first`. Correct but unhelpful; the
  agent has no machine-readable signal or remediation hint. 1/6 real calls hit
  it on first use in a fresh repo.

## 4. Options Considered

### Same-session filtering (G1)

| Option | Verdict |
|---|---|
| Extension passes current session id to CLI; exclude at channel level | **Chosen.** The extension already runs in-process with `ctx.sessionManager.getSessionId()`; one indexed SQL lookup per query. Channel-level exclusion (FTS `NOT IN` / vector post-filter) keeps pool slots for useful units. |
| Down-rank self-session instead of dropping | Rejected as default: the conversation is already in the model's context; any rank wastes budget. |
| Filter inside runtime admission only | Rejected: self units would still consume `top_n` pool slots before admission. |
| Env var (`SNOOP_EXCLUDE_SESSION`) instead of flag | Unnecessary indirection; the extension builds the argv. |

### Timestamp rendering (G4)

| Option | Verdict |
|---|---|
| `3h ago` / `2026-08-30 (2d ago)` / `2025-01-03` | **Chosen.** Age-first, absolute anchor when age alone loses meaning, ~6-25 chars. |
| Full ISO-8601 (`2026-08-31T18:04:59Z`) | Precise but 20 chars of noise; model must still compute age. |
| Date only (`2026-08-31`) | Loses intra-day ordering for active sessions. |
| Relative only (`2d`) | Drifts between calls; no absolute anchor. |
| New `chrono`/`time` dependency | Rejected: `src/ingest/harness/jsonl.rs` already hand-rolls days-from-civil; the inverse is ~25 LoC. |

Rules: `< 1h` -> `Nm ago`; `< 24h` -> `Nh ago`; `< 365d` -> `YYYY-MM-DD (Nd ago)`;
else `YYYY-MM-DD`. Computed against query time; missing timestamp omits the
field. Raw epoch stays in `--explain` diagnostics for tests.

### Flooding (G3)

| Option | Verdict |
|---|---|
| Per-source admission cap (default 3) in the runtime | **Chosen.** One counter map keyed by `source_id`; applies to commits, files, and sessions alike; lexical and hybrid. |
| Lexical near-dup suppression (shingle overlap within same source) | Good follow-up if cap=3 still leaks near-identical hunks; more code, defer. |
| Render-time grouping of same-source items into one item | Changes the packet contract; defer. |
| Raise `top_n` | Treats the symptom; flooded slots still dominate RRF. |

### Ordering (G5)

Sort the final admitted items by fused rank (`selection_order`), keeping
required-role picks pinned ahead only when they would otherwise be absent.
Simplest correct version: stable-sort all accepted items by their fused rank;
role floor still guarantees presence, just not position.

### Not-indexed error (G6)

Emit `{"status": "error", "error": "index a repository first", "hint":
"run: snoop init ."}` on the error path for query/inspect in `main.rs`.

### Deferred ideas (not planned)

- Prompt-echo down-ranking for cross-session episodes (containment check
  between query text and episode user text).
- Per-turn assistant-summary units to replace raw transcripts (needs reindex
  policy bump; largest change, weakest evidence of need so far).

## 5. Implementation Plan

Constraints: no new dependencies, schema unchanged, all CLI/MCP changes
additive, each phase independently shippable and testable.

### Phase 1: Same-session exclusion (G1) - P0

| Step | File | Change |
|---|---|---|
| 1.1 | `src/store/queries.rs` | Add `unit_ids_for_anchor(kind, value)` helper (no limit) or reuse `units_for_anchor` with `ANCHOR_LOOKUP_LIMIT`. |
| 1.2 | `src/runtime.rs` | Add `exclude_unit_ids: HashSet<i64>` (default empty) to `QueryOptions`. Filter each channel's id list before fusion. |
| 1.3 | `src/main.rs` | `Query` and `Inspect` gain repeatable `--exclude-session <ID>`; resolve ids via session anchor, pass into options. |
| 1.4 | `src/mcp/mod.rs` + `serve.rs` | Optional `exclude_sessions: string[]` argument on both tools; same resolution. |
| 1.5 | `extensions/snoop-pi.ts` | In both `execute` fns: read `ctx.sessionManager.getSessionId()`, append `--exclude-session <id>`. |
| 1.6 | `README.md` | Document flag/arg and the rationale (self-hits). |

Tests: runtime test that excluded ids vanish from channels and a same-sized
replacement candidate fills the pool slot; CLI test for flag plumbing; MCP test
for the optional argument.

### Phase 2: Human timestamps (G4) - P0

| Step | File | Change |
|---|---|---|
| 2.1 | `src/metadata.rs` (`timestamp` mod) | Add `render(epoch: i64, now: i64) -> String` with the rules above; hand-rolled civil-from-days (inverse of `jsonl.rs` conversion). |
| 2.2 | `src/core.rs` | `ContextItem.timestamp` becomes `Option<String>`; add `now: i64` to `QueryOptions` (default: system time) so tests are deterministic. |
| 2.3 | `src/runtime.rs` | Render at item construction. |
| 2.4 | `src/mcp/mod.rs` | `symbol_context_entries`: render timestamps for GitCommit *and* AgentSession entries. |
| 2.5 | `src/main.rs` | `sessions` and `inspect unit` output rendered; keep raw epoch in `--explain` diagnostics only. |

Tests: `render` table test (minutes/hours/days/date boundaries, far past,
missing, now-injected); packet JSON shows rendered string; symbol entries
include session timestamps.

### Phase 3: Per-source admission cap (G3) - P1

| Step | File | Change |
|---|---|---|
| 3.1 | `src/runtime.rs` | In `admit`, track `admitted_per_source: HashMap<i64, usize>`; reject when count >= `max_per_source` (new `QueryOptions` field, default 3). Exempt the per-facet required pick so the role floor can never be blocked by the cap. |
| 3.2 | `README.md` | Document the diversity cap. |

Tests: packet with a 15-hunk commit admits <= 3 and frees budget to other
sources; required-role pick survives the cap; regression of the C2/C4 shape.

### Phase 4: Relevance ordering (G5) - P2

| Step | File | Change |
|---|---|---|
| 4.1 | `src/runtime.rs` | Stable-sort `accepted_ids` by fused rank from `selection_order` before rendering. |

Tests: output order matches rank order for fill items; role-floor items still
present.

### Phase 5: Not-indexed error payload (G6) - P2

| Step | File | Change |
|---|---|---|
| 5.1 | `src/main.rs` | Query/Inspect error path prints JSON `{"status":"error","error":...,"hint":"run: snoop init ."}` and exits non-zero. |

Tests: CLI test on an unindexed temp dir.

### Suggested order and validation

1. Phases 1+2 together (both P0, touch the same call sites once).
2. Phase 3; replay C1-C5 queries and confirm: zero self-session items after
   exclusion, no source > 3 items, top item no longer a prompt echo.
3. Phases 4+5.
4. Validation commands: `cargo test`, then the `analyze_grc.py` scanner re-run
   against new sessions after a day of use; spot-check with
   `snoop query ... --explain` for admission reasons.

### Explicitly out of scope

- Ingest policy changes to episode transcripts (needs `TURN_POLICY_VERSION`
  bump and full reindex; only revisit if G2 persists after Phase 1).
- Embedding/retrieval quality changes; all fixes are admission/render-time.
