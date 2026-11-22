// Claude lane (ADR-0085 decisions 4 and 8, AC-1424): the Claude Agent SDK
// drives the loop. Its own tools, settings, plugins and prompts are off; the
// only tools are the role's, served in process and executed by the host.
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { CallToolRequestSchema, ListToolsRequestSchema } from "@modelcontextprotocol/sdk/types.js";
import type { HostChannel } from "./host.ts";
import type { SidecarMessage, StartMessage } from "./protocol.ts";

export const SERVER_NAME = "cartograph";

/** One MCP server whose tools are exactly the role's, with the host's JSON Schemas. */
export function cartographServer(start: StartMessage, host: HostChannel): McpServer {
  const server = new McpServer({ name: SERVER_NAME, version: "1" }, { capabilities: { tools: {} } });
  server.server.setRequestHandler(ListToolsRequestSchema, async () => ({
    tools: start.tools.map((tool) => ({ name: tool.name, description: tool.description, inputSchema: tool.input_schema as any })),
  }));
  server.server.setRequestHandler(CallToolRequestSchema, async (request) => {
    const result = await host.callTool(request.params.name, request.params.arguments ?? {});
    return { content: [{ type: "text", text: result.content }], isError: result.is_error };
  });
  return server;
}

/** SDK options. Everything that could reach past the role's tools is closed. */
export function claudeOptions(start: StartMessage, host: HostChannel, abort: AbortController) {
  const allowed = start.tools.map((tool) => `mcp__${SERVER_NAME}__${tool.name}`);
  const deny = (reason: string) => ({
    hookSpecificOutput: { hookEventName: "PreToolUse" as const, permissionDecision: "deny" as const, permissionDecisionReason: reason },
  });
  const postTool = async (input: any, content: string, isError: boolean) => {
    const name = String(input?.tool_name ?? "").slice(`mcp__${SERVER_NAME}__`.length);
    await host.hook("post_tool", { tool: name, arguments: input?.tool_input, result: { content, is_error: isError } });
    if (host.finished) abort.abort();
    return {};
  };
  return {
    model: start.model,
    systemPrompt: start.system,
    abortController: abort,
    maxTurns: start.max_turns,
    // No built-in tools at all; only the in-process server's.
    tools: [] as string[],
    allowedTools: allowed,
    mcpServers: { [SERVER_NAME]: { type: "sdk" as const, name: SERVER_NAME, instance: cartographServer(start, host) } },
    strictMcpConfig: true,
    settingSources: [] as never[],
    permissionMode: "dontAsk" as const,
    canUseTool: async (name: string) =>
      allowed.includes(name)
        ? { behavior: "allow" as const, updatedInput: undefined as any }
        : { behavior: "deny" as const, message: "Only the role's Cartograph tools are available." },
    // The SDK reaches only the host's loopback proxy, with the per-run token.
    // The real key, if any, is added by the host (ADR-0085 decision 7).
    env: {
      ANTHROPIC_BASE_URL: start.base_url,
      ANTHROPIC_API_KEY: start.token,
      CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
      DISABLE_TELEMETRY: "1",
      DISABLE_AUTOUPDATER: "1",
      PATH: process.env.PATH ?? "",
    },
    hooks: {
      PreToolUse: [
        {
          hooks: [
            async (input: any) => {
              const name = String(input?.tool_name ?? "");
              if (!allowed.includes(name)) return deny("Only the role's Cartograph tools are available.");
              const outcome = await host.hook("pre_tool", { tool: name.slice(`mcp__${SERVER_NAME}__`.length), arguments: input?.tool_input });
              return outcome.allow ? {} : deny(outcome.reason ?? "Denied by the run's pre_tool hook.");
            },
          ],
        },
      ],
      PostToolUse: [{ hooks: [async (input: any) => postTool(input, JSON.stringify(input?.tool_response ?? ""), false)] }],
      // A failed or interrupted call (a host refusal included) is a separate SDK
      // event; forward it so post_tool sees every executed call, as on the Pi lane.
      PostToolUseFailure: [{ hooks: [async (input: any) => postTool(input, String(input?.error ?? ""), true)] }],
      Stop: [{ hooks: [async () => (await host.hook("stop"), {})] }],
    },
  };
}

export async function runClaude(
  start: StartMessage,
  host: HostChannel,
  done: (message: Extract<SidecarMessage, { type: "done" }>) => void,
): Promise<void> {
  const { query } = await import("@anthropic-ai/claude-agent-sdk");
  const abort = new AbortController();
  host.whenCancelled(() => abort.abort());
  const runStart = await host.hook("run_start");
  if (!runStart.allow) {
    done({ type: "done", reason: "stopped", detail: runStart.reason });
    return;
  }
  let reason: "finished" | "stopped" | "max_turns" | "cancelled" | "error" = "stopped";
  let detail: string | undefined;
  try {
    for await (const message of query({ prompt: start.prompt, options: claudeOptions(start, host, abort) as any })) {
      if ((message as any).type === "result") {
        const subtype = (message as any).subtype as string;
        if (subtype === "error_max_turns") reason = "max_turns";
        else if (subtype !== "success") {
          reason = "error";
          detail = subtype;
        }
      }
    }
  } catch (error) {
    if (!host.finished && !host.isCancelled) {
      reason = "error";
      detail = String(error);
    }
  }
  if (host.isCancelled) reason = "cancelled";
  else if (host.finished) reason = "finished";
  done({ type: "done", reason, detail });
}
