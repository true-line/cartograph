// Claude lane (ADR-0085 decisions 4 and 8, AC-1424): the Claude Agent SDK
// drives the loop. Its own tools, settings, plugins and prompts are off; the
// only tools are the role's, served in process and executed by the host.
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { CallToolRequestSchema, ListToolsRequestSchema } from "@modelcontextprotocol/sdk/types.js";
import type { HostChannel } from "./host.ts";
import type { SidecarMessage, StartMessage } from "./protocol.ts";

export const SERVER_NAME = "cartograph";

/** One MCP server whose tools are exactly the role's, with the host's JSON Schemas. */
export function cartographServer(start: StartMessage, host: HostChannel, stop: () => void = () => {}): McpServer {
  const server = new McpServer({ name: SERVER_NAME, version: "1" }, { capabilities: { tools: {} } });
  server.server.setRequestHandler(ListToolsRequestSchema, async () => ({
    tools: start.tools.map((tool) => ({ name: tool.name, description: tool.description, inputSchema: tool.input_schema as any })),
  }));
  server.server.setRequestHandler(CallToolRequestSchema, async (request) => {
    const result = await host.callTool(request.params.name, request.params.arguments ?? {});
    // An admitted finish ends the run here, while Claude Code still waits for
    // this result, so it cannot make another model request.
    if (host.finished) stop();
    return { content: [{ type: "text", text: result.content }], isError: result.is_error };
  });
  return server;
}

/**
 * SDK options. Everything that could reach past the role's tools is closed.
 * `home` is the run's own empty directory: Claude Code's working directory,
 * home and config, so it reads nothing of the user's and leaves nothing behind.
 */
export function claudeOptions(
  start: StartMessage,
  host: HostChannel,
  abort: AbortController,
  home = tmpdir(),
  stop: () => void = () => abort.abort(),
) {
  const allowed = start.tools.map((tool) => `mcp__${SERVER_NAME}__${tool.name}`);
  const deny = (reason: string) => ({
    hookSpecificOutput: { hookEventName: "PreToolUse" as const, permissionDecision: "deny" as const, permissionDecisionReason: reason },
  });
  const postTool = async (input: any, content: string, isError: boolean) => {
    const name = String(input?.tool_name ?? "").slice(`mcp__${SERVER_NAME}__`.length);
    await host.hook("post_tool", { tool: name, arguments: input?.tool_input, result: { content, is_error: isError } });
    // An admitted finish ends the run: Claude Code is stopped before it can
    // make another model request (neither an abort signal nor a hook's
    // `continue: false` stops its loop).
    if (host.finished) {
      stop();
      return { continue: false, stopReason: "The host admitted the finish." };
    }
    return {};
  };
  return {
    model: start.model,
    systemPrompt: start.system,
    // The host-verified Claude Code (AC-1424); never one found on PATH.
    pathToClaudeCodeExecutable: start.claude_executable,
    cwd: home,
    abortController: abort,
    maxTurns: start.max_turns,
    // No built-in tools at all; only the in-process server's.
    tools: [] as string[],
    allowedTools: allowed,
    mcpServers: { [SERVER_NAME]: { type: "sdk" as const, name: SERVER_NAME, instance: cartographServer(start, host, stop) } },
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
      // The host's per-step output limit; the proxy refuses a larger request.
      CLAUDE_CODE_MAX_OUTPUT_TOKENS: String(start.max_output_tokens ?? 4096),
      HOME: home,
      CLAUDE_CONFIG_DIR: join(home, ".claude"),
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
  if (!start.claude_executable) {
    done({ type: "done", reason: "error", detail: "no Claude Code executable was given" });
    return;
  }
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
  const home = mkdtempSync(join(tmpdir(), "cartograph-claude-"));
  try {
    let running: (AsyncIterable<unknown> & { close(): void }) | undefined;
    const close = () => {
      abort.abort();
      running?.close();
    };
    host.whenCancelled(close);
    running = query({ prompt: start.prompt, options: claudeOptions(start, host, abort, home, close) as any });
    for await (const message of running) {
      if (host.finished || host.isCancelled) break;
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
  rmSync(home, { recursive: true, force: true });
  if (host.isCancelled) reason = "cancelled";
  else if (host.finished) reason = "finished";
  done({ type: "done", reason, detail });
}
