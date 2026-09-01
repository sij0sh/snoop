// snoop-pi.ts -- pre-launch repository indexing and evidence tools for pi sessions.
//
// On session start (startup, new, resume, fork) this spawns `snoop ensure`
// detached in the session directory and never awaits it, so launches stay
// instant and freshness is eventual. For a blocking pre-launch check use
// `snoop ensure . --timeout 30 && exec pi` instead.
//
// Also registers two read-only tools backed by the snoop CLI, mirroring the
// snoop MCP server for pi sessions that do not attach MCP:
//   get_repo_context      -> snoop query <query> --tokens <max_tokens>
//   repo_symbol_context   -> snoop inspect symbol <symbol>
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

export default function snoopPi(pi: ExtensionAPI) {
  pi.registerTool({
    name: "get_repo_context",
    label: "Repo Context",
    description:
      "Investigate a repository question across current code, docs, git history, and prior agent work. Returns a token-budgeted evidence packet.",
    promptSnippet:
      "Investigate a repository question across code, docs, git history, and prior agent work",
    promptGuidelines: [
      "Default to get_repo_context for repository investigation and understanding questions.",
      "Use direct tools when their exact output is needed for the next action.",
    ],
    parameters: Type.Object({
      query: Type.String({
        description: "What you want to understand about the repository.",
      }),
      max_tokens: Type.Optional(
        Type.Number({ description: "Maximum context to return. Default 6000." }),
      ),
    }),
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      const text = await runSnoop(
        pi,
        ["query", params.query, "--tokens", String(params.max_tokens ?? DEFAULT_MAX_TOKENS)],
        ctx.cwd,
        signal,
      );
      return { content: [{ type: "text", text }], details: {} };
    },
  });

  pi.registerTool({
    name: "repo_symbol_context",
    label: "Symbol Context",
    description:
      "Get repository context for a known symbol across code, docs, commits, and prior agent work.",
    promptSnippet: "Get repository context for a known symbol",
    promptGuidelines: [
      "Use repo_symbol_context when the user names a specific symbol.",
    ],
    parameters: Type.Object({
      symbol: Type.String({
        description: "Symbol to investigate, e.g. refresh_session.",
      }),
    }),
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      const text = await runSnoop(pi, ["inspect", "symbol", params.symbol], ctx.cwd, signal);
      return { content: [{ type: "text", text }], details: {} };
    },
  });

  pi.on("session_start", async (event, ctx) => {
    // "reload" re-enters the same session; refresh once per session entry.
    if (event.reason === "reload") return;
    if (process.env.SNOOP_ENSURE === "0") return;

    const cwd = ctx.cwd;
    const timeout = process.env.SNOOP_ENSURE_TIMEOUT || DEFAULT_TIMEOUT_SECS;
    const logPath = join(cwd, ".snoop-ensure.log");

    const child = spawn("snoop", ["ensure", "--timeout", timeout], {
      cwd,
      stdio: "ignore",
      detached: true,
    });
    child.on("error", (error) => {
      // Diagnosable, never silent: missing binary, permissions, spawn errors.
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
  });
}
