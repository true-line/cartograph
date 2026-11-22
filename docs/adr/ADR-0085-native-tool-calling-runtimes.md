# ADR-0085 — Agents research with native tool calls: Pi for local models, the Claude Agent SDK for Claude

- **Status:** Proposed
- **Date:** 2026-11-22
- **Deciders:** Cartograph owner (direction); project lead as delegate; Claude implements
- **Related:** ADR-0066 (amends decisions 3 and 9, settles "Not decided here"); ADR-0064; ADR-0071; ADR-0076; ADR-0081; ADR-0083; #1335; #1352; #1382; AC-1404; AC-1405

## Context

Investigation roles (analyst, auditor and the rest of `crates/agents`) run in
one bounded loop the host drives. Each turn the model returns one JSON object
naming one action from a closed set (`query_context`,
`read_coverage_inventory`, `read_evidence`, `replay_query`, `finish`); the
host decodes it against the turn's schema
(`ProgressiveInvestigationAction::decode_resolved_detailed_for_specialist`),
admits it through the job journal and builds the next prompt. Every provider
reports `ProviderCaps.tool_use: false`, and `crates/llm/src/bounded.rs`
rejects a reply that carries native `tool_calls`.

On 2026-11-22 the analyst acceptances D1–D3 (#1335, #1352, #1382) failed on
`qwen3:30b-a3b`: a context cap (prompt truncation), a query loop, and a finish
after one query that cited evidence the run never acquired. A one-action JSON
chat cannot plan a sequence of reads, see a tool error and recover, or keep
its own working context; the host re-serialises the whole history into each
prompt instead. The owner's direction: agents must research with real tools,
open-source models through Pi Agent, Claude through the Anthropic Agent SDK.

ADR-0066 (Proposed) already places SDK runtimes in the app as supervised
sidecars, serves them only the role's tools, keeps one admission gate and
makes cloud consent per run. It kept local models on the bounded loop and left
the sidecar language and packaging open.

Facts checked for this decision (2026-11-22):

- **Pi** (`@mariozechner/pi-agent-core`, `@mariozechner/pi-ai`, 0.73.x): MIT.
  TypeScript agent loop with typed tools (TypeBox schemas), tool-call events
  and an OpenAI-compatible provider that reaches Ollama. No MCP client is
  needed: tools are registered in process.
- **Claude Agent SDK** (`@anthropic-ai/claude-agent-sdk`, 0.3.x): proprietary,
  under Anthropic's commercial terms (license "SEE LICENSE IN README"), Node
  >= 18. Custom tools are served by an in-process MCP server
  (`createSdkMcpServer`); built-in tools can be turned off; the API base URL is
  configurable (`ANTHROPIC_BASE_URL`).
- Ollama `/api/chat` accepts `tools` and returns `tool_calls` for qwen3 and
  flint; the flint MLX runner rejects `format`, which tool calling does not use.

## Decision

1. **Every role's actions become native tools.** The five actions are a closed
   tool vocabulary (`agents::investigation::native_tools::AgentTool`), with
   argument JSON Schemas generated from the same per-turn action schema the
   bounded loop uses. A role is offered exactly its capability set (ADR-0071
   table); listing a tool grants nothing by itself.
2. **The host executes every tool call through the existing admission.** A
   call's name and arguments are decoded into the same
   `ProgressiveInvestigationAction` and admitted through the same worker path
   (so the repeated-read and repeated-query guards apply) and the same journal,
   budgets, read limits, citation provenance and `egress-read-deny/v1`
   (ADR-0083) as a bounded-loop action. Admitted steps and refusals are written
   to the same records (ADR-0076, AC-1404, AC-1405). A refused call returns to
   the model as a tool error carrying the refusal code and a prescription; it
   does not end the run. An unknown tool, invalid arguments or an over-budget
   call is a refusal, never a panic or a silent skip.
3. **Gates live in the tools.** A `finish` with findings is refused unless
   the run has admitted at least one `read_evidence` and every citation it
   names was acquired in this run; an insufficient-evidence `finish` needs at
   least one admitted query (read-before-finish). The refused finish text is
   retained for scoring (AC-1404 record). Further gates are added the same way:
   a check inside the tool that refuses with feedback.
4. **The harness owns hooks; Cartograph's workflow layer sits above it.**
   Universal-agentic-workflow is a layer on top of an agent harness and does
   not re-enforce what the harness already handles. So hooks are the
   harness's own: Agent SDK hooks (`PreToolUse`, `PostToolUse`, `Stop` and the
   rest it exposes) on the Claude lane, and Pi's agent events and tool-call
   interception on the local lane. The frozen run definition declares the
   hooks by lifecycle point; the sidecar installs them in that harness, a
   point the harness does not expose is refused at freeze, and every hook
   outcome is reported to the host and ledgered. A hook may allow or deny
   with context for the model and never rewrites a call or result; a deny is
   a refusal inside the attempt and consumes no attempt. Cartograph's
   workflow layer adds only stages, gates (decision 3), routing and rollback
   (decision 5). The repeated-read/query guard and read-before-finish reach
   the model through the harness's `PreToolUse` as a deny with context, and
   the same host check runs again at admission, so the hook gives early
   feedback and the host stays authoritative. Only the in-app bounded loop,
   which has no harness, keeps its own copy of these checks. Hooks are not a
   safety boundary: admission, budgets and the firewall stay in the host
   (decisions 2 and 7), so a hook that is skipped or misbehaves cannot widen
   what a run can read or send.
5. **Rollback-and-resume is host-driven and counts as one attempt.** The host
   checkpoints the conversation at each admitted step. When a declared trigger
   fires (a finish refused by a gate, or N consecutive refusals), the host
   rolls the runtime back to the last checkpoint and resumes it with the
   prescription (base task + refusal + attempts left). Every rollback is
   ledgered; the cap and the triggers are part of the frozen run definition.
   Over the cap the run ends with the recorded reason. A rollback inside these
   bounds is the same attempt; anything else is a new attempt.
6. **One TypeScript sidecar, two runners.** `agent-runtime/` builds one
   `cartograph-agent` executable per platform, shipped as a Tauri external
   binary and signed with the app. It holds the Pi runner (local models) and
   the Agent SDK runner (Claude). The host starts one process per run, speaks
   newline-delimited JSON-RPC over stdio (run start, tool call/result, events,
   checkpoint, rollback, cancel) and kills it on cancel, deadline or limit.
   Tools reach the host over that pipe, not over a socket; `crates/mcp-local`
   stays the door for outside agents (ADR-0066 decision 6).
7. **The sidecar has no key and no network.** (Amends ADR-0066 decision 3.)
   The host runs a loopback model proxy per run, bound to 127.0.0.1 with a
   per-run token. The Pi runner's Ollama base URL and the Agent SDK's
   `ANTHROPIC_BASE_URL` point at it. The proxy applies the egress firewall to
   every request (redaction, tier policy, ADR-0083 read-deny, byte and time
   limits, the frozen `:num-ctx-<n>` of ADR-0081 for Ollama) and adds the
   provider key itself; the key never enters the sidecar. Any other origin,
   path or token is refused.
8. **The Agent SDK's own tools are off.** Built-in tools are disabled, only
   `mcp__cartograph__*` tools are allowed, no settings, hooks or plugins are
   loaded from disk, and permission prompts resolve to deny. A
   Claude run can only call the role's tools.
9. **Local models use the Pi runner.** (Amends ADR-0066 decision 9.) New
   tool-calling role versions run on Pi for Ollama and on the Agent SDK for
   Claude. The bounded JSON loop stays for role versions that already exist and
   for acceptance freezes that name them (ADR-0071); it is not removed here.
10. **Consent.** Local runs need none. Cloud tool runs use ADR-0066 decision 7:
    consent per run, stated before the run, with every request the proxy sends
    recorded by size and hash in the run record.
11. **Record.** The run record names the runtime and its version
    (`pi-agent-core@x`, `claude-agent-sdk@x`), the model, the provider id and
    the frozen definition (tools, hooks, gates, rollback cap).

## Consequences

- Agents can chain reads, see refusals and recover inside a run; the host no
  longer rebuilds one giant prompt per step, which removes the context-cap
  failure mode of D1.
- Safety moves to one place, the host tool boundary plus the model proxy,
  and both are runtime-independent: a new runtime gets the same checks.
- The app ships and signs a Node-based executable per platform, and its size
  grows. The Agent SDK is proprietary; it is a runtime dependency, listed in
  THIRD-PARTY-NOTICES with its terms, and the user supplies their own key.
- Per-step payload preview is gone for cloud tool runs (as ADR-0066 accepted);
  the proxy's request record replaces it.
- D1–D3 are rerun on new role versions under fresh frozen definitions; the
  2026-11-22 failures stay recorded against the bounded loop.

## Alternatives rejected

- **Native tool calling in Rust on the direct APIs.** Fastest to build, but it
  is a fourth loop of our own, not Pi or the Agent SDK, and ADR-0066 already
  rejected reimplementing the SDK loops.
- **Tools over `crates/mcp-local`'s socket.** It is built for outside clients
  with grants and handshakes; a per-run stdio pipe owned by the host is a
  smaller surface with the same handlers.
- **Hand the provider key to the sidecar (ADR-0066 decision 3 as written).**
  The sidecar would then reach the provider directly, outside the firewall.
