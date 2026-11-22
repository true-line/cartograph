// AC-1422, AC-1423, AC-1427 (ADR-0085): the real executable over stdio, with Pi
// speaking Ollama's native chat wire or vLLM's OpenAI-compatible wire to a
// stand-in for the host proxy.
// The proxy sees the per-run token, never a provider key, and every tool call
// comes back to the host as a protocol message.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer, type IncomingMessage } from "node:http";
import { once } from "node:events";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const main = fileURLToPath(new URL("../src/main.ts", import.meta.url));
// AC-1427: with CARTOGRAPH_AGENT_EXECUTABLE set, the same tests run the
// packaged single executable (built by scripts/build-agent-runtime.mjs)
// instead of the sources.
const packaged = process.env.CARTOGRAPH_AGENT_EXECUTABLE;
const command: [string, string[]] = packaged ? [packaged, []] : [process.execPath, [main]];

function ndjson(res: any, lines: object[]) {
  res.writeHead(200, { "content-type": "application/x-ndjson" });
  for (const line of lines) res.write(`${JSON.stringify(line)}\n`);
  res.end();
}

function toolCallLines(name: string, args: object) {
  return [
    { model: "qwen3:30b-a3b", message: { role: "assistant", content: "", tool_calls: [{ function: { name, arguments: args } }] }, done: false },
    { model: "qwen3:30b-a3b", message: { role: "assistant", content: "" }, done: true, done_reason: "stop", prompt_eval_count: 120, eval_count: 9 },
  ];
}

function sse(res: any, chunks: object[]) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  for (const chunk of chunks) res.write(`data: ${JSON.stringify(chunk)}\n\n`);
  res.end("data: [DONE]\n\n");
}

function sseToolCall(id: string, name: string, args: object) {
  const base = { id: `chatcmpl-${id}`, object: "chat.completion.chunk", created: 0, model: "glm-5.3-flash" };
  return [
    { ...base, choices: [{ index: 0, delta: { role: "assistant", tool_calls: [{ index: 0, id, type: "function", function: { name, arguments: JSON.stringify(args) } }] }, finish_reason: null }] },
    { ...base, choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] },
  ];
}

async function body(req: IncomingMessage): Promise<any> {
  let text = "";
  for await (const chunk of req) text += chunk;
  return JSON.parse(text);
}

interface ProxyRequest { url: string; auth: string | undefined; body: any }

/** Spawns the real executable against a stand-in proxy that answers each model request. */
async function runProcess(answer: (res: any, step: number) => void, extra: object) {
  const requests: ProxyRequest[] = [];
  const proxy = createServer(async (req, res) => {
    const payload = await body(req);
    requests.push({ url: req.url ?? "", auth: req.headers.authorization, body: payload });
    answer(res, requests.length);
  });
  proxy.listen(0, "127.0.0.1");
  await once(proxy, "listening");
  const port = (proxy.address() as any).port;

  const child = spawn(command[0], command[1], { stdio: ["pipe", "pipe", "inherit"], env: { PATH: process.env.PATH ?? "" } });
  const received: any[] = [];
  let buffered = "";
  const write = (message: object) => child.stdin.write(`${JSON.stringify(message)}\n`);
  const finished = new Promise<any>((resolve) => {
    child.stdout.on("data", (data) => {
      buffered += data;
      let newline;
      while ((newline = buffered.indexOf("\n")) >= 0) {
        const message = JSON.parse(buffered.slice(0, newline));
        buffered = buffered.slice(newline + 1);
        received.push(message);
        if (message.type === "tool_call") {
          const terminal = message.name === "finish";
          write({ type: "tool_result", call_id: message.call_id, content: terminal ? "admitted" : "page 1: 3 facts", is_error: false, terminal });
        }
        if (message.type === "done") resolve(message);
      }
    });
  });
  write({
    type: "start",
    run_id: "run-7",
    lane: "pi",
    model: "qwen3:30b-a3b",
    base_url: `http://127.0.0.1:${port}/run-7`,
    token: "run-7-token",
    system: "system text",
    prompt: "task text",
    tools: [
      { name: "query_context", description: "q", input_schema: { type: "object", properties: { query: { type: "object" } }, required: ["query"] } },
      { name: "finish", description: "f", input_schema: { type: "object", properties: { findings: { type: "array" } }, required: ["findings"] } },
    ],
    hooks: [],
    max_turns: 8,
    ...extra,
  });
  const done = await finished;
  child.stdin.end();
  await once(child, "exit");
  proxy.close();
  assert.equal(received[0].type, "ready");
  assert.match(received[0].runners.pi, /^pi-agent-core@\d/, "the ready line names the installed Pi runner");
  // The packaged build has no Agent SDK lane yet (AC-1424), so it names none.
  if (packaged) assert.equal(received[0].runners.claude, "");
  else assert.match(received[0].runners.claude, /^claude-agent-sdk@\d/);
  assert.deepEqual(received.filter((m) => m.type === "tool_call").map((m) => m.name), ["query_context", "finish"]);
  assert.equal(done.reason, "finished");
  assert.equal(requests.length, 2, "one model request per turn, none after the admitted finish");
  for (const request of requests) {
    assert.equal(request.auth, "Bearer run-7-token", "the proxy token, never a provider key");
    assert.deepEqual(request.body.tools.map((t: any) => t.function.name), ["query_context", "finish"], "native tools on the wire");
    assert.equal(request.body.messages[0].role, "system");
  }
  return requests;
}

test("cartograph-agent runs a Pi loop over stdio against the loopback proxy (Ollama)", async () => {
  const requests = await runProcess(
    (res, step) =>
      ndjson(res, step === 1 ? toolCallLines("query_context", { query: { scope: { type: "all" } } }) : toolCallLines("finish", { findings: [] })),
    { context_window: 32768 },
  );
  for (const request of requests) {
    assert.equal(request.url, "/run-7/api/chat");
    assert.equal(request.body.model, "qwen3:30b-a3b");
    assert.equal(request.body.options.num_ctx, 32768, "the frozen context window on every request");
  }
  const second = requests[1]!.body.messages;
  assert.equal(second.at(-1).role, "tool");
  assert.equal(second.at(-1).content, "page 1: 3 facts", "the host's tool result goes back to the model");
  assert.equal(second.at(-1).tool_name, "query_context");
  assert.deepEqual(second.at(-2).tool_calls, [{ function: { name: "query_context", arguments: { query: { scope: { type: "all" } } } } }]);
});

test("cartograph-agent runs a Pi loop against vLLM's OpenAI-compatible API (DGX Spark lane)", async () => {
  const requests = await runProcess(
    (res, step) =>
      sse(res, step === 1 ? sseToolCall("t1", "query_context", { query: { scope: { type: "all" } } }) : sseToolCall("t2", "finish", { findings: [] })),
    { model: "glm-5.3-flash", backend: "vllm", context_window: 65536, max_output_tokens: 2048 },
  );
  for (const request of requests) {
    assert.equal(request.url, "/run-7/v1/chat/completions");
    assert.equal(request.body.model, "glm-5.3-flash");
    assert.equal(request.body.max_tokens, 2048);
    assert.equal(request.body.stream, true);
    assert.equal(request.body.chat_template_kwargs.enable_thinking, false, "thinking off through the chat template");
    assert.equal(request.body.options, undefined, "no Ollama options on the vLLM wire");
    assert.equal(request.body.store, undefined);
    for (const tool of request.body.tools) assert.equal(tool.function.strict, undefined);
  }
  const second = requests[1]!.body.messages;
  assert.equal(second.at(-1).role, "tool");
  assert.equal(second.at(-1).tool_call_id, "t1");
  assert.equal(second.at(-1).content, "page 1: 3 facts");
  assert.equal(second.at(-2).tool_calls[0].function.name, "query_context");
});
