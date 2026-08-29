// snoop-pi.ts -- pre-launch repository indexing for pi sessions.
//
// On session start (startup, new, resume, fork) this spawns `snoop ensure`
// detached in the session directory and never awaits it, so launches stay
// instant and freshness is eventual. For a blocking pre-launch check use
// `snoop ensure . --timeout 30 && exec pi` instead.
//
// Install: copy into ~/.pi/agent/extensions/ or <project>/.pi/extensions/.
// Disable: SNOOP_ENSURE=0. Budget: SNOOP_ENSURE_TIMEOUT (seconds, default 120).
// Spawn failures are appended to .snoop-ensure.log in the project directory.

import { spawn } from "node:child_process";
import { appendFileSync } from "node:fs";
import { join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const DEFAULT_TIMEOUT_SECS = "120";

export default function snoopEnsureOnSessionStart(pi: ExtensionAPI) {
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
