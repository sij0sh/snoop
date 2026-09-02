// snoop-pi.ts -- pre-launch repository indexing and evidence tools for pi sessions.
//
// On session start and after compaction this spawns `snoop ensure` detached
// in the session directory and never awaits it, so launches stay instant and
// compacted-away conversation becomes queryable. For a blocking check use
// `snoop ensure . --timeout 30 && exec pi` instead.
//
// Also registers the read-only `context` tool backed by `snoop query`,
// mirroring the snoop MCP server for pi sessions that do not attach MCP.
//
// Install: copy into ~/.pi/agent/extensions/ or <project>/.pi/extensions/.
// Disable ensure: SNOOP_ENSURE=0. Budget: SNOOP_ENSURE_TIMEOUT (seconds, default 120).
// Spawn failures are appended to .snoop-ensure.log in the project directory.

import { spawn } from "node:child_process";
import { appendFileSync } from "node:fs";
import { join } from "node:path";
import { Type } from "typebox";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const DEFAULT_TIMEOUT_SECS = "120";
const TOOL_TIMEOUT_MS = 60_000;
const DEFAULT_MAX_TOKENS = 6_000;

interface SnoopExecResult {
  stdout: string;
  stderr: string;
  code: number;
  killed: boolean;
}

async function runSnoop(
  pi: ExtensionAPI,
  args: string[],
  cwd: string,
  signal?: AbortSignal,
): Promise<string> {
  let result: SnoopExecResult;
  try {
    result = await pi.exec("snoop", args, { cwd, signal, timeout: TOOL_TIMEOUT_MS });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`snoop exec failed: ${message}. Is the snoop binary installed?`);
  }
  const note = result.stderr.trim();
  const text = result.stdout + (note ? `\n${note}` : "");
  if (result.code !== 0) {
    throw new Error(text.trim() || `snoop ${args.join(" ")} exited with code ${result.code}`);
  }
  return text;
}

function spawnEnsure(cwd: string) {
  if (process.env.SNOOP_ENSURE === "0") return;

  const timeout = process.env.SNOOP_ENSURE_TIMEOUT || DEFAULT_TIMEOUT_SECS;
  const logPath = join(cwd, ".snoop-ensure.log");
  const child = spawn("snoop", ["ensure", "--timeout", timeout], {
    cwd,
    stdio: "ignore",
    detached: true,
  });
  child.on("error", (error) => {
    try {
      appendFileSync(
        logPath,
        `${new Date().toISOString()} snoop ensure spawn failed: ${error.message}\n`,
      );
    } catch {
      // A failed log write must never break a session.
    }
  });
  child.unref();
}

export default function snoopPi(pi: ExtensionAPI) {
  pi.registerTool({
    name: "context",
    label: "Repository Context",
    description:
      "Get relevant repository context across code, docs, git history, and prior agent work.",
    promptSnippet:
      "Get relevant repository context across code, docs, git history, and prior agent work",
    promptGuidelines: [
      "Use context for repository investigation and understanding questions.",
      "Use direct tools when their exact output is needed for the next action.",
    ],
    parameters: Type.Object({
      query: Type.String({
        description: "What you need to understand.",
      }),
      max_tokens: Type.Optional(
        Type.Number({ description: "Maximum context to return. Default 6000." }),
      ),
    }),
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      const args = [
        "query",
        params.query,
        "--tokens",
        String(params.max_tokens ?? DEFAULT_MAX_TOKENS),
      ];
      const text = await runSnoop(pi, args, ctx.cwd, signal);
      return { content: [{ type: "text", text }], details: {} };
    },
  });

  pi.on("session_start", async (event, ctx) => {
    // "reload" re-enters the same session; refresh once per session entry.
    if (event.reason !== "reload") spawnEnsure(ctx.cwd);
  });

  pi.on("session_compact", async (_event, ctx) => {
    spawnEnsure(ctx.cwd);
  });
}
