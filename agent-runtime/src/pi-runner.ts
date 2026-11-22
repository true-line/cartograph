// Local lane (ADR-0085 decision 9, AC-1423): Pi drives the loop against the
// host's loopback proxy, which forwards to Ollama's native chat API so the
// frozen context window (ADR-0081) is sent on every request.
import { Agent, type AgentTool, type StreamFn } from "@mariozechner/pi-agent-core";
import type { Model } from "@mariozechner/pi-ai";
import type { HostChannel } from "./host.ts";
import { ollamaStreamFn } from "./ollama-stream.ts";
import type { SidecarMessage, StartMessage } from "./protocol.ts";

export function piModel(start: StartMessage): Model<"openai-completions"> {
  return {
    id: start.model,
    name: start.model,
    api: "openai-completions",
    provider: "ollama",
    baseUrl: start.base_url,
    reasoning: false,
    input: ["text"],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: start.context_window ?? 32768,
    maxTokens: start.max_output_tokens ?? 4096,
  };
}

/** Tools whose execution is entirely the host's. */
export function piTools(start: StartMessage, host: HostChannel): AgentTool<any>[] {
  return start.tools.map((spec) => ({
    name: spec.name,
    label: spec.name,
    description: spec.description,
    parameters: spec.input_schema as any,
    async execute(_id: string, params: unknown) {
      const result = await host.callTool(spec.name, params);
      if (result.is_error) {
        // Pi reports a thrown error to the model as an error tool result.
        throw new Error(result.content);
      }
      return { content: [{ type: "text" as const, text: result.content }], details: undefined, terminate: result.terminal };
    },
  }));
}

export async function runPi(
  start: StartMessage,
  host: HostChannel,
  done: (message: Extract<SidecarMessage, { type: "done" }>) => void,
  streamFn?: StreamFn,
): Promise<void> {
  let turns = 0;
  const agent = new Agent({
    initialState: { systemPrompt: start.system, model: piModel(start), tools: piTools(start, host), thinkingLevel: "off" },
    // The proxy token, not a provider key: the host adds any real credential.
    getApiKey: () => start.token,
    toolExecution: "sequential",
    streamFn: (streamFn ?? ollamaStreamFn(start.base_url, start.token, start.context_window)) as StreamFn,
    beforeToolCall: async ({ toolCall, args }) => {
      const outcome = await host.hook("pre_tool", { tool: toolCall.name, arguments: args });
      return outcome.allow ? undefined : { block: true, reason: outcome.reason ?? "Denied by the run's pre_tool hook." };
    },
    afterToolCall: async ({ toolCall, args, result, isError }) => {
      const text = result.content.map((part) => (part.type === "text" ? part.text : "")).join("");
      await host.hook("post_tool", { tool: toolCall.name, arguments: args, result: { content: text, is_error: isError } });
      // The turn this call belongs to is `turns + 1`; stop at the declared cap.
      return host.finished || turns + 1 >= start.max_turns ? { terminate: true } : undefined;
    },
  });
  agent.subscribe((event) => {
    if (event.type === "turn_end") turns++;
  });
  host.whenCancelled(() => agent.abort());
  const runStart = await host.hook("run_start");
  if (!runStart.allow) {
    done({ type: "done", reason: "stopped", detail: runStart.reason });
    return;
  }
  try {
    await agent.prompt(start.prompt);
    await agent.waitForIdle();
  } catch (error) {
    done({ type: "done", reason: "error", detail: String(error) });
    return;
  }
  await host.hook("stop");
  if (host.isCancelled) done({ type: "done", reason: "cancelled" });
  else if (host.finished) done({ type: "done", reason: "finished" });
  else if (turns >= start.max_turns) done({ type: "done", reason: "max_turns" });
  else done({ type: "done", reason: "stopped", detail: agent.state.errorMessage });
}
