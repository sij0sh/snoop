---
cheatcodes_id: 18adb1adba
type: Decision
title: Share symbol and history context builders
description: Shared builders in mcp.rs provide symbol and history context to both MCP tools and the CLI.
tags:
  - mcp
  - cli
  - refactoring
status: draft
generated:
  by: cheatcodes/0.2.0
  at: 2026-08-29T17:51:10.068Z
sources:
  - id: session-f1e8c51548867110
    resource: session:01a03fa1-5327-77d2-9330-0370ebf5cf69#entries=d5b522bd
    title: Session evidence
---

# Answer

Use symbol_context_entries and history_entries from mcp.rs as shared builders for MCP and CLI callers.

# Why

Moving the builders into mcp.rs removes duplicated loops from main.rs while preserving CLI inspect symbol and history behavior.

# Evidence

- [evidence-5c8b4f877df4be1f1e02b539] Validation failed for tool "workflow_transition":
  - checkpoint.summary: must have required properties summary
  - checkpoint: must not have additional properties

Received arguments:
{
  "checkpoint": {
    "data": {
      "docs": [
        "README.md (V1 scope + MCP tool table)",
        ".pi-files/build-plan.md (phase 16 status + V1 verdict)",
        ".pi-files/v1-close.md"
      ],
      "mcp": {
        "async_gate": "stay-sync, no tokio",
        "protocol": "2025-06-18",
        "tools": [
          "get_repo_context",
          "repo_symbol_context",
          "repo_history"
        ],
        "validation": "15 suites, 66 passed, 2 ignored; clippy -D warnings clean; manual smoke test on snoop itself answered a critic question with 13 items at 792/800 tokens"
      },
      "simplification": "symbol_context_entries and history_entries moved to mcp.rs as shared builders; CLI inspect symbol and history now call them, deleting the duplicated loops from main.rs",
      "tests": "t
[truncated]
