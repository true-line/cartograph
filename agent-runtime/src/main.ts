// cartograph-agent: one process per run, started and stopped by the host.
// stdin: host messages, one JSON object per line. stdout: sidecar messages.
// Nothing else is written to stdout; diagnostics go to stderr.
import { readFileSync } from "node:fs";
import { findPackageJSON } from "node:module";
import { createInterface } from "node:readline";
import { HostChannel } from "./host.ts";
import { parseHostMessage, type RunnerVersions, type SidecarMessage, type StartMessage } from "./protocol.ts";

export const RUNTIME_VERSION = "1";

/**
 * `<package>@<installed version>` for a dependency, read from the package.json
 * of the copy this process actually loads, so the run record names the
 * harness that drove it rather than this bridge's protocol version.
 */
export function packageVersion(name: string, from: string = import.meta.url): string {
  const file = findPackageJSON(name, from);
  const manifest = file ? (JSON.parse(readFileSync(file, "utf8")) as { name?: unknown; version?: unknown }) : {};
  if (manifest.name !== name || typeof manifest.version !== "string" || manifest.version === "") {
    throw new Error(`no installed version for ${name}`);
  }
  return `${name.slice(name.lastIndexOf("/") + 1)}@${manifest.version}`;
}

export interface Io {
  lines: AsyncIterable<string>;
  write(line: string): void;
}

export async function serve(io: Io, runners: {
  pi: (start: StartMessage, host: HostChannel, done: (m: Extract<SidecarMessage, { type: "done" }>) => void) => Promise<void>;
  claude: (start: StartMessage, host: HostChannel, done: (m: Extract<SidecarMessage, { type: "done" }>) => void) => Promise<void>;
}, versions: RunnerVersions): Promise<Extract<SidecarMessage, { type: "done" }>> {
  const send = (message: SidecarMessage) => io.write(`${JSON.stringify(message)}\n`);
  send({ type: "ready", runtime: "cartograph-agent", version: RUNTIME_VERSION, runners: versions });
  let host: HostChannel | undefined;
  let finished: Extract<SidecarMessage, { type: "done" }> | undefined;
  let run: Promise<void> | undefined;
  const done = (message: Extract<SidecarMessage, { type: "done" }>) => {
    if (finished) return;
    finished = message;
    send(message);
  };
  for await (const line of io.lines) {
    if (line.trim() === "") continue;
    let message;
    try {
      message = parseHostMessage(line);
    } catch (error) {
      done({ type: "done", reason: "error", detail: `protocol: ${(error as Error).message}` });
      break;
    }
    if (message.type === "start") {
      if (host) {
        done({ type: "done", reason: "error", detail: "protocol: second start" });
        break;
      }
      host = new HostChannel({ send }, new Set(message.hooks));
      run = runners[message.lane](message, host, done).catch((error) =>
        done({ type: "done", reason: "error", detail: String(error) }),
      );
      continue;
    }
    if (!host) {
      done({ type: "done", reason: "error", detail: "protocol: message before start" });
      break;
    }
    try {
      host.receive(message);
    } catch (error) {
      done({ type: "done", reason: "error", detail: `protocol: ${(error as Error).message}` });
      break;
    }
    if (finished) break;
  }
  if (run) await Promise.race([run, new Promise((resolve) => setTimeout(resolve, 1000))]);
  return finished ?? { type: "done", reason: "cancelled", detail: "host closed stdin" };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const { runPi } = await import("./pi-runner.ts");
  const { runClaude } = await import("./claude-runner.ts");
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  const versions = {
    pi: packageVersion("@mariozechner/pi-agent-core"),
    claude: packageVersion("@anthropic-ai/claude-agent-sdk"),
  };
  await serve({ lines, write: (line) => process.stdout.write(line) }, { pi: runPi, claude: runClaude }, versions);
  process.exit(0);
}
