// ADR-0085 decision 6: newline-delimited JSON over stdio between the Rust host
// and this sidecar. The host owns every tool, check, key and network path; the
// sidecar only runs the harness loop (Pi or the Claude Agent SDK).

export type Lane = "pi" | "claude";

/** Lifecycle points a frozen definition may declare (ADR-0085 decision 4). */
export type HookPoint = "run_start" | "pre_tool" | "post_tool" | "stop";
export const HOOK_POINTS: readonly HookPoint[] = ["run_start", "pre_tool", "post_tool", "stop"];

export interface ToolSpec {
  name: string;
  description: string;
  input_schema: Record<string, unknown>;
}

/** Host -> sidecar: the only message that starts a run. */
export interface StartMessage {
  type: "start";
  run_id: string;
  lane: Lane;
  model: string;
  /** Per-run loopback model proxy, e.g. http://127.0.0.1:PORT/RUN (AC-1422). */
  base_url: string;
  /** Per-run proxy token. Never a provider key. */
  token: string;
  system: string;
  prompt: string;
  tools: ToolSpec[];
  hooks: HookPoint[];
  max_turns: number;
  context_window?: number;
  max_output_tokens?: number;
  /**
   * Pi lane only: the server behind the proxy. `ollama` (default) speaks the
   * native chat API with `num_ctx`; `vllm` speaks the OpenAI-compatible API,
   * whose context window is fixed at server start (`max_model_len`).
   */
  backend?: PiBackend;
}

export type PiBackend = "ollama" | "vllm";

export interface ToolResultMessage {
  type: "tool_result";
  call_id: string;
  content: string;
  is_error: boolean;
  /** The host admitted a finish: the run ends after this result. */
  terminal: boolean;
}

export interface HookResultMessage {
  type: "hook_result";
  hook_id: string;
  decision: "allow" | "deny";
  reason?: string;
}

export interface CancelMessage {
  type: "cancel";
}

export type HostMessage = StartMessage | ToolResultMessage | HookResultMessage | CancelMessage;

/** Sidecar -> host. */
export type SidecarMessage =
  | { type: "ready"; runtime: string; version: string; runners: RunnerVersions }
  | { type: "tool_call"; call_id: string; name: string; arguments: unknown }
  | {
      type: "hook";
      hook_id: string;
      point: HookPoint;
      tool?: string;
      arguments?: unknown;
      result?: { content: string; is_error: boolean };
    }
  | { type: "done"; reason: "finished" | "stopped" | "max_turns" | "cancelled" | "error"; detail?: string };

const MAX_LINE_BYTES = 4 * 1024 * 1024;

/** Parses one host line; anything not in the closed vocabulary is refused. */
export function parseHostMessage(line: string): HostMessage {
  if (Buffer.byteLength(line) > MAX_LINE_BYTES) throw new Error("host message too large");
  const value = JSON.parse(line) as Record<string, unknown>;
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("host message must be an object");
  }
  switch (value.type) {
    case "start":
      return validateStart(value);
    case "tool_result":
      if (typeof value.call_id !== "string" || typeof value.content !== "string") {
        throw new Error("invalid tool_result");
      }
      return {
        type: "tool_result",
        call_id: value.call_id,
        content: value.content,
        is_error: value.is_error === true,
        terminal: value.terminal === true,
      };
    case "hook_result":
      if (typeof value.hook_id !== "string" || (value.decision !== "allow" && value.decision !== "deny")) {
        throw new Error("invalid hook_result");
      }
      return {
        type: "hook_result",
        hook_id: value.hook_id,
        decision: value.decision,
        reason: typeof value.reason === "string" ? value.reason : undefined,
      };
    case "cancel":
      return { type: "cancel" };
    default:
      throw new Error("unknown host message type");
  }
}

function validateStart(value: Record<string, unknown>): StartMessage {
  const strings = ["run_id", "model", "base_url", "token", "system", "prompt"] as const;
  for (const key of strings) {
    if (typeof value[key] !== "string") throw new Error(`start.${key} must be a string`);
  }
  if (value.lane !== "pi" && value.lane !== "claude") throw new Error("start.lane must be pi or claude");
  const url = new URL(value.base_url as string);
  if (url.protocol !== "http:" || (url.hostname !== "127.0.0.1" && url.hostname !== "[::1]")) {
    throw new Error("start.base_url must be the host's loopback proxy");
  }
  if (!Array.isArray(value.tools) || value.tools.length === 0) throw new Error("start.tools must be non-empty");
  const tools = value.tools.map((tool: any) => {
    if (typeof tool?.name !== "string" || typeof tool?.description !== "string" || typeof tool?.input_schema !== "object") {
      throw new Error("invalid tool spec");
    }
    return { name: tool.name, description: tool.description, input_schema: tool.input_schema };
  });
  const hooks = Array.isArray(value.hooks) ? value.hooks : [];
  for (const hook of hooks) {
    if (!HOOK_POINTS.includes(hook)) throw new Error(`hook point not exposed by this runtime: ${String(hook)}`);
  }
  const backend = value.backend ?? "ollama";
  if (backend !== "ollama" && backend !== "vllm") throw new Error("start.backend must be ollama or vllm");
  if (value.backend !== undefined && value.lane !== "pi") throw new Error("start.backend applies to the pi lane only");
  const maxTurns = value.max_turns;
  if (typeof maxTurns !== "number" || !Number.isInteger(maxTurns) || maxTurns < 1 || maxTurns > 256) {
    throw new Error("start.max_turns must be 1..256");
  }
  return {
    type: "start",
    run_id: value.run_id as string,
    lane: value.lane,
    model: value.model as string,
    base_url: value.base_url as string,
    token: value.token as string,
    system: value.system as string,
    prompt: value.prompt as string,
    tools,
    hooks: hooks as HookPoint[],
    max_turns: maxTurns,
    context_window: typeof value.context_window === "number" ? value.context_window : undefined,
    max_output_tokens: typeof value.max_output_tokens === "number" ? value.max_output_tokens : undefined,
    backend,
  };
}

/** The installed harness behind each lane, such as `pi-agent-core@0.73.1`. */
export interface RunnerVersions {
  pi: string;
  claude: string;
}
