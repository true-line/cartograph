// The packaged entry (ADR-0085 decision 6, AC-1427): bundled with its locked
// dependencies into the signed `cartograph-agent` executable the app ships.
// The runner versions are fixed at build time from the lockfile, since a
// single executable has no node_modules to read them from.
import { createInterface } from "node:readline";
import { serve } from "./main.ts";
import { runPi } from "./pi-runner.ts";
import type { RunnerVersions } from "./protocol.ts";

declare const CARTOGRAPH_RUNNERS: RunnerVersions;

// The Agent SDK lane is not in this build yet (AC-1424). Its version is empty,
// so the host refuses a Claude run before it starts; this is the backstop.
const claude: Parameters<typeof serve>[1]["claude"] = async (_start, _host, done) =>
  done({ type: "done", reason: "error", detail: "the Claude lane is not in this build" });

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
serve({ lines, write: (line) => process.stdout.write(line) }, { pi: runPi, claude }, CARTOGRAPH_RUNNERS).then(
  () => process.exit(0),
  (error) => {
    process.stderr.write(`${String(error)}\n`);
    process.exit(1);
  },
);
