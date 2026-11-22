// AC-1423, AC-1424, AC-1425 (ADR-0085): the sidecar runs the harness loop and
// nothing else. Every tool call and declared hook is a host round trip.
import assert from "node:assert/strict";
import { test } from "node:test";
import { createAssistantMessageEventStream, type AssistantMessage, type Context } from "@mariozechner/pi-ai";
import { claudeOptions } from "../src/claude-runner.ts";
import { HostChannel } from "../src/host.ts";
import { serve } from "../src/main.ts";
import { runPi } from "../src/pi-runner.ts";
import { parseHostMessage, type SidecarMessage, type StartMessage } from "../src/protocol.ts";

const tools = [
  { name: "query_context", description: "q", input_schema: { type: "object", properties: { query: { type: "object" } }, required: ["query"] } },
  { name: "read_evidence", description: "r", input_schema: { type: "object", properties: { index: { type: "integer" } }, required: ["index"] } },
  { name: "finish", description: "f", input_schema: { type: "object", properties: { findings: { type: "array" } }, required: ["findings"] } },
];

function start(overrides: Partial<StartMessage> = {}): StartMessage {
  return {
    type: "start",
    run_id: "run-1",
    lane: "pi",
    model: "qwen3:30b-a3b",
    base_url: "http://127.0.0.1:41234/run-1",
    token: "proxy-token",
    system: "You are a bounded Cartograph investigation specialist.",
    prompt: "Investigate checkout.",
    tools,
    hooks: [],
    max_turns: 12,
    ...overrides,
  };
}

/** A scripted model: each turn returns the next tool call and records what it saw. */
function scriptedModel(calls: Array<{ name: string; arguments: Record<string, unknown> } | { text: string }>) {
  const seen: Context[] = [];
  let turn = 0;
  const streamFn = (model: any, context: Context) => {
    seen.push({ systemPrompt: context.systemPrompt, messages: structuredClone(context.messages) } as Context);
    const step = calls[turn++] ?? { text: "done" };
    const stream = createAssistantMessageEventStream();
    const message: AssistantMessage = {
      role: "assistant",
      content: "text" in step ? [{ type: "text", text: step.text }] : [{ type: "toolCall", id: `tc-${turn}`, name: step.name, arguments: step.arguments }],
      api: model.api,
      provider: model.provider,
      model: model.id,
      usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
      stopReason: "text" in step ? "stop" : "toolUse",
      timestamp: Date.now(),
    };
    queueMicrotask(() => {
      stream.push({ type: "start", partial: message });
      stream.push({ type: "done", reason: "text" in step ? "stop" : "toolUse", message });
    });
    return stream;
  };
  return { streamFn: streamFn as any, seen };
}

/** A fake host: answers tool calls and hooks from a policy and records traffic. */
function fakeHost(policy: {
  tool: (name: string, args: any) => { content: string; is_error?: boolean; terminal?: boolean };
  hook?: (point: string, tool?: string) => { decision: "allow" | "deny"; reason?: string };
}, hooks: StartMessage["hooks"] = []) {
  const sent: SidecarMessage[] = [];
  let channel!: HostChannel;
  channel = new HostChannel(
    {
      send(message) {
        sent.push(message);
        queueMicrotask(() => {
          if (message.type === "tool_call") {
            const answer = policy.tool(message.name, message.arguments);
            channel.receive({ type: "tool_result", call_id: message.call_id, content: answer.content, is_error: !!answer.is_error, terminal: !!answer.terminal });
          } else if (message.type === "hook") {
            const answer = policy.hook?.(message.point, message.tool) ?? { decision: "allow" };
            channel.receive({ type: "hook_result", hook_id: message.hook_id, ...answer });
          }
        });
      },
    },
    new Set(hooks),
  );
  return { channel, sent };
}

function toolResultTexts(context: Context): string[] {
  return context.messages
    .filter((message: any) => message.role === "toolResult")
    .map((message: any) => `${message.isError ? "error:" : ""}${message.content.map((part: any) => part.text).join("")}`);
}

test("protocol refuses a non-loopback model endpoint and undeclared hook points", () => {
  const line = (value: object) => JSON.stringify({ ...start(), ...value });
  assert.throws(() => parseHostMessage(line({ base_url: "https://api.anthropic.com" })), /loopback/);
  assert.throws(() => parseHostMessage(line({ hooks: ["on_compact"] })), /not exposed/);
  assert.throws(() => parseHostMessage(line({ lane: "openai" })), /lane/);
  assert.throws(() => parseHostMessage(JSON.stringify({ type: "exec", command: "ls" })), /unknown/);
  assert.equal(parseHostMessage(line({})).type, "start");
});

test("Pi lane: every tool call is executed by the host and refusals are fed back", async () => {
  const model = scriptedModel([
    { name: "bash", arguments: { command: "cat /etc/passwd" } },
    { name: "query_context", arguments: { query: { scope: { type: "all" } } } },
    { name: "finish", arguments: { findings: [{ title: "early" }] } },
    { name: "read_evidence", arguments: { index: 0 } },
    { name: "finish", arguments: { findings: [{ title: "supported" }] } },
  ]);
  let reads = 0;
  const host = fakeHost({
    tool(name) {
      if (name === "read_evidence") reads++;
      if (name === "finish" && reads === 0) return { content: "read_before_finish: call read_evidence first", is_error: true };
      if (name === "finish") return { content: "admitted", terminal: true };
      return { content: `${name} page` };
    },
  });
  let done: any;
  await runPi(start(), host.channel, (message) => (done = message), model.streamFn);
  const calls = host.sent.filter((m) => m.type === "tool_call").map((m: any) => m.name);
  assert.deepEqual(calls, ["query_context", "finish", "read_evidence", "finish"], "an unoffered tool never reaches the host");
  assert.equal(done.reason, "finished");
  assert.equal(model.seen.length, 5, "the loop stops when the host admits a finish");
  const fed = toolResultTexts(model.seen[4]!);
  assert.match(fed[0]!, /^error:.*bash/, "the harness refuses an unoffered tool to the model");
  assert.equal(fed[1], "query_context page");
  assert.match(fed[2]!, /^error:read_before_finish/, "a host refusal is a tool error, and the run continues");
  assert.equal(fed[3], "read_evidence page");
  assert.equal(model.seen[0]!.systemPrompt, start().system);
});

test("Pi lane: a declared pre_tool hook denies through the harness without a host tool call", async () => {
  const model = scriptedModel([
    { name: "query_context", arguments: { query: { scope: { type: "all" } } } },
    { name: "query_context", arguments: { query: { scope: { type: "all" } } } },
    { name: "finish", arguments: { findings: [] } },
  ]);
  let queries = 0;
  const host = fakeHost(
    {
      tool: (name) => (name === "finish" ? { content: "admitted", terminal: true } : { content: "page" }),
      hook: (point, tool) => (point === "pre_tool" && tool === "query_context" && queries++ > 0
        ? { decision: "deny", reason: "repeated_read: inspect the existing result" }
        : { decision: "allow" }),
    },
    ["pre_tool", "post_tool", "run_start", "stop"],
  );
  let done: any;
  await runPi(start({ hooks: ["pre_tool", "post_tool", "run_start", "stop"] }), host.channel, (m) => (done = m), model.streamFn);
  assert.equal(done.reason, "finished");
  assert.deepEqual(host.sent.filter((m) => m.type === "tool_call").map((m: any) => m.name), ["query_context", "finish"]);
  const points = host.sent.filter((m) => m.type === "hook").map((m: any) => m.point);
  assert.equal(points[0], "run_start");
  assert.equal(points.at(-1), "stop");
  assert.equal(points.filter((p) => p === "pre_tool").length, 3);
  assert.equal(points.filter((p) => p === "post_tool").length, 2, "post_tool runs for executed calls only");
  assert.match(toolResultTexts(model.seen[2]!)[1]!, /repeated_read/);
});

test("Pi lane: max_turns stops a run that never finishes", async () => {
  const model = scriptedModel(Array.from({ length: 10 }, () => ({ name: "query_context", arguments: { query: {} } })));
  const host = fakeHost({ tool: () => ({ content: "page" }) });
  let done: any;
  await runPi(start({ max_turns: 3 }), host.channel, (m) => (done = m), model.streamFn);
  assert.equal(done.reason, "max_turns");
  assert.equal(model.seen.length, 3);
});

test("Claude lane: built-in tools, settings and other servers are off; the key stays in the host", async () => {
  const host = fakeHost({ tool: () => ({ content: "page" }) });
  const options = claudeOptions(start({ lane: "claude", model: "claude-sonnet-5-5" }), host.channel, new AbortController());
  assert.deepEqual(options.tools, []);
  assert.deepEqual(options.allowedTools, ["mcp__cartograph__query_context", "mcp__cartograph__read_evidence", "mcp__cartograph__finish"]);
  assert.deepEqual(options.settingSources, []);
  assert.equal(options.strictMcpConfig, true);
  assert.deepEqual(Object.keys(options.mcpServers), ["cartograph"]);
  assert.equal(options.env.ANTHROPIC_BASE_URL, "http://127.0.0.1:41234/run-1");
  assert.equal(options.env.ANTHROPIC_API_KEY, "proxy-token");
  assert.equal(Object.keys(options.env).filter((key) => /KEY|TOKEN|SECRET/.test(key) && key !== "ANTHROPIC_API_KEY").length, 0);
  assert.equal((await options.canUseTool("Bash")).behavior, "deny");
  assert.equal((await options.canUseTool("mcp__cartograph__finish")).behavior, "allow");
  const pre = options.hooks.PreToolUse[0]!.hooks[0]!;
  const denied: any = await pre({ tool_name: "WebFetch", tool_input: {} });
  assert.equal(denied.hookSpecificOutput.permissionDecision, "deny");
});

test("Claude lane: post_tool sees failed calls as errors, like the Pi lane", async () => {
  const host = fakeHost({ tool: () => ({ content: "page" }) }, ["post_tool"]);
  const options = claudeOptions(start({ lane: "claude", model: "claude-sonnet-5-5", hooks: ["post_tool"] }), host.channel, new AbortController());
  await options.hooks.PostToolUse[0]!.hooks[0]!({ tool_name: "mcp__cartograph__query_context", tool_input: { query: {} }, tool_response: "page" });
  await options.hooks.PostToolUseFailure[0]!.hooks[0]!({ tool_name: "mcp__cartograph__finish", tool_input: { findings: [] }, error: "read_before_finish: call read_evidence first" });
  const posts = host.sent.filter((m) => m.type === "hook" && m.point === "post_tool") as any[];
  assert.deepEqual(posts.map((m) => [m.tool, m.result.is_error]), [["query_context", false], ["finish", true]]);
  assert.match(posts[1].result.content, /read_before_finish/);
});

test("serve: refuses traffic before start and a second start", async () => {
  async function* lines(...items: string[]) {
    yield* items;
  }
  const out: string[] = [];
  const io = (items: string[]) => ({ lines: lines(...items), write: (line: string) => out.push(line) });
  const never = async () => {};
  const before = await serve(io([JSON.stringify({ type: "cancel" })]), { pi: never, claude: never });
  assert.match(before.detail ?? "", /before start/);
  const twice = await serve(io([JSON.stringify(start()), JSON.stringify(start())]), { pi: never, claude: never });
  assert.match(twice.detail ?? "", /second start/);
  assert.equal(JSON.parse(out[0]!).type, "ready");
});
