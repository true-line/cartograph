// The packaged entry (ADR-0085 decision 6, AC-1427): bundled with its locked
// dependencies into the signed `cartograph-agent` executable the app ships.
// The runner versions are fixed at build time from the lockfile, since a
// single executable has no node_modules to read them from.
import { createInterface } from "node:readline";
import { serve } from "./main.ts";
import { runClaude } from "./claude-runner.ts";
import { runPi } from "./pi-runner.ts";
import type { RunnerVersions } from "./protocol.ts";

declare const CARTOGRAPH_RUNNERS: RunnerVersions;

// Both lanes are bundled. The Claude lane drives the Claude Code executable
// the host names in its start message (AC-1424).
const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
serve({ lines, write: (line) => process.stdout.write(line) }, { pi: runPi, claude: runClaude }, CARTOGRAPH_RUNNERS).then(
  () => process.exit(0),
  (error) => {
    process.stderr.write(`${String(error)}\n`);
    process.exit(1);
  },
);
