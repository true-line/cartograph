# agent-runtime — the `cartograph-agent` sidecar (ADR-0085)

One process per investigation run, started, fed and stopped by the desktop
host. It runs the agent harness loop and nothing else:

- **Pi** (`@mariozechner/pi-agent-core`, MIT) for local models: Ollama through
  its native chat API (the frozen `num_ctx` on every request), or vLLM through
  its OpenAI-compatible API (`backend: "vllm"`, the DGX Spark lane; the
  server needs `--enable-auto-tool-choice` and a `--tool-call-parser`).
- **Claude Agent SDK** (`@anthropic-ai/claude-agent-sdk`, Anthropic commercial
  terms) for Claude.

The host owns everything that matters for safety: every tool is executed by the
host through the same admission, budgets, read limits and egress rules as the
bounded loop; the model endpoint is the host's per-run loopback proxy
(`127.0.0.1`, per-run token), which applies the egress firewall and adds any
provider key itself. The sidecar never holds a key and has no other network
path. The Agent SDK's built-in tools, settings sources and other MCP servers
are off.

## Protocol

Newline-delimited JSON on stdin (host → sidecar) and stdout (sidecar → host);
diagnostics go to stderr. Types are in `src/protocol.ts`.

| Direction | Message | Meaning |
|-----------|---------|---------|
| → host | `ready` | Process is up; names runtime and version. |
| ← host | `start` | Lane (`pi` or `claude`), Pi backend (`ollama` or `vllm`), model, proxy URL and token, system and task prompt, the role's tools with JSON Schemas, declared hook points, turn cap. |
| → host | `tool_call` | The model called a role tool; the host admits or refuses it. |
| ← host | `tool_result` | Result text, or an error the model sees as a tool error; `terminal` when the host admitted a finish. |
| → host | `hook` | A declared lifecycle point (`run_start`, `pre_tool`, `post_tool`, `stop`) reached in the harness. |
| ← host | `hook_result` | `allow` or `deny` with a reason; a deny never rewrites a call. |
| ← host | `cancel` | Stop now. |
| → host | `done` | `finished`, `stopped`, `max_turns`, `cancelled` or `error`. |

Hooks are the harness's own (Pi `beforeToolCall`/`afterToolCall`, Agent SDK
`PreToolUse`/`PostToolUse`/`Stop`); the host decides each one and re-checks at
admission, so a hook is early feedback, never the safety boundary.

## Develop

```sh
npm install
npm test        # protocol, both lanes, and a real Pi loop over stdio
npm run typecheck
```

Node >= 22.18 runs the TypeScript sources directly (type stripping).
Packaging as a signed Tauri external binary is a later ADR-0085 slice.
