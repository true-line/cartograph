// AC-1422, AC-1423, AC-1427 (ADR-0085): the real executable over stdio, with Pi
// speaking Ollama's native chat wire to a stand-in for the host proxy.
// The proxy sees the per-run token, never a provider key, and every tool call
// comes back to the host as a protocol message.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer, type IncomingMessage } from "node:http";
import { once } from "node:events";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const main = fileURLToPath(new URL("../src/main.ts", import.meta.url));

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

async function body(req: IncomingMessage): Promise<any> {
  let text = "";
  for await (const chunk of req) text += chunk;
  return JSON.parse(text);
}

test("cartograph-agent runs a Pi loop over stdio against the loopback proxy", async () => {
  const requests: Array<{ url: string; auth: string | undefined; body: any }> = [];
  const proxy = createServer(async (req, res) => {
    const payload = await body(req);
    requests.push({ url: req.url ?? "", auth: req.headers.authorization, body: payload });
    const step = requests.length;
    if (step === 1) ndjson(res, toolCallLines("query_context", { query: { scope: { type: "all" } } }));
    else ndjson(res, toolCallLines("finish", { findings: [] }));
  });
  proxy.listen(0, "127.0.0.1");
  await once(proxy, "listening");
  const port = (proxy.address() as any).port;

  const child = spawn(process.execPath, [main], { stdio: ["pipe", "pipe", "inherit"], env: { PATH: process.env.PATH ?? "" } });
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
    context_window: 32768,
  });
  const done = await finished;
  child.stdin.end();
  await once(child, "exit");
  proxy.close();

  assert.equal(received[0].type, "ready");
  assert.deepEqual(received.filter((m) => m.type === "tool_call").map((m) => m.name), ["query_context", "finish"]);
  assert.equal(done.reason, "finished");
  assert.equal(requests.length, 2, "one model request per turn, none after the admitted finish");
  for (const request of requests) {
    assert.equal(request.url, "/run-7/api/chat");
    assert.equal(request.auth, "Bearer run-7-token");
    assert.equal(request.body.model, "qwen3:30b-a3b");
    assert.deepEqual(request.body.tools.map((t: any) => t.function.name), ["query_context", "finish"], "native tools on the wire");
    assert.equal(request.body.options.num_ctx, 32768, "the frozen context window on every request");
    assert.equal(request.body.messages[0].role, "system");
  }
  const second = requests[1]!.body.messages;
  assert.equal(second.at(-1).role, "tool");
  assert.equal(second.at(-1).content, "page 1: 3 facts", "the host's tool result goes back to the model");
  assert.equal(second.at(-1).tool_name, "query_context");
  assert.deepEqual(second.at(-2).tool_calls, [{ function: { name: "query_context", arguments: { query: { scope: { type: "all" } } } } }]);
});
