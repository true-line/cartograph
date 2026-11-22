// cartograph-agent: one process per run, started and stopped by the host.
// stdin: host messages, one JSON object per line. stdout: sidecar messages.
// Nothing else is written to stdout; diagnostics go to stderr.
import { createInterface } from "node:readline";
import { HostChannel } from "./host.ts";
import { parseHostMessage, type SidecarMessage, type StartMessage } from "./protocol.ts";

export const RUNTIME_VERSION = "1";

export interface Io {
  lines: AsyncIterable<string>;
  write(line: string): void;
}

export async function serve(io: Io, runners: {
  pi: (start: StartMessage, host: HostChannel, done: (m: Extract<SidecarMessage, { type: "done" }>) => void) => Promise<void>;
  claude: (start: StartMessage, host: HostChannel, done: (m: Extract<SidecarMessage, { type: "done" }>) => void) => Promise<void>;
}): Promise<Extract<SidecarMessage, { type: "done" }>> {
  const send = (message: SidecarMessage) => io.write(`${JSON.stringify(message)}\n`);
  send({ type: "ready", runtime: "cartograph-agent", version: RUNTIME_VERSION });
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
  await serve({ lines, write: (line) => process.stdout.write(line) }, { pi: runPi, claude: runClaude });
  process.exit(0);
}
