# ADR-0066 — The agent team runs in the app; outside agents reach it through MCP

- **Status:** Proposed; owner review
- **Date:** 2026-11-17
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1745; #1748; #1763; ROADMAP §11 ("Providers"); W8b; ADR-0036; ADR-0056; ADR-0057; ADR-0061; ADR-0064; AC-0670

## Context

The agent team is part of the product. The deterministic engine recovers
the facts; the agent team writes the ADRs, the specifications and the rest
of the document set backwards from those facts, and keeps them current
(VISION V10, ADR-0056). The desktop app is the human seat: every agent
output is cited, staged and reviewed there.

The roadmap leaves one provider question open: how the team's agents run
on Anthropic and OpenAI models. The direction is to run them through the
providers' agent SDKs with the user's own keys, beside local open-source
models.

What exists today:

- **Roles.** A team member is a versioned role definition
  (`crates/agents`, such as `section-writer@1`): its prompt, the tools it
  may use and the finish it must produce. A finish is staged only when the
  pure admission rule accepts it (ADR-0064).
- **One runtime.** Roles run in a bounded loop the host drives, over
  `crates/llm`: a local Ollama server, the Anthropic Messages API and the
  OpenAI API. Cloud requests pass the egress firewall with consent per
  step.
- **Keys.** A provider key is stored in the operating system's credential
  store (ADR-0057, #1763).
- **MCP.** `crates/mcp-local` is the door for outside agents such as Codex
  and Claude Code. Under a host grant they read what Cartograph holds
  (`cartograph_discover`, `cartograph_read_fact`, `cartograph_artifacts`,
  `cartograph_read_artifact`, `cartograph_evidence`) and work with the
  in-app agents: start a role's task, follow it, read its result and
  cancel it (`cartograph_task_start`, `_status`, `_events`, `_result`,
  `_cancel`).

What the agent SDKs are: the Claude Agent SDK and the OpenAI Agents SDK
are Python and TypeScript packages. Neither has a Rust binding. Each runs
an agent loop, with the user's key, and each can use MCP servers as tools.
(To verify against the vendors' current documentation when the runtime is
built.)

An earlier draft of this ADR placed SDK agents outside the app, as MCP
clients that submit their own drafts. That was wrong on both counts and is
withdrawn: the agents are in the app, and MCP is how outside agents
interact with them.

## Decision

1. **A role is defined once and runs on a runtime the user picks.** The
   runtimes are: a local model (the existing bounded loop), the Claude
   Agent SDK, and the OpenAI Agents SDK. The role's prompt, tool list and
   finish contract are the same on each. The runtime and the model are
   recorded with every output.
2. **The app hosts the SDK runtimes.** The app starts, supervises and
   stops a runtime process of its own for an SDK run (a sidecar shipped
   with the app). The user starts a run from the app, as for any role, and
   never installs or launches anything else.
3. **The key comes from the credential store.** The app hands the user's
   provider key to the runtime process for the length of the run, in
   memory, and writes it nowhere (ADR-0057). With no key, the runtime is
   not offered.
4. **An agent's tools are the role's and nothing else.** The host serves
   the runtime process the role's own tools (the bounded, redacted reads
   of facts, artifacts and evidence, and the role's finish) for that run
   only. The SDKs' own file, shell and web tools are off. The agent can
   only propose.
5. **Every output passes the same gate.** A draft, a finding or a
   proposal from an SDK run is admitted by the same pure rule as one from
   the local loop (ADR-0064 decisions 3 and 4), is T3 / InferredWeak, is
   staged, and reaches a document only after human review (R-INT-1,
   R-INT-3).
6. **Outside agents work with the team through MCP.** Codex, Claude Code
   or another harness asks an in-app agent to do the work: it starts a
   role's task over MCP (a drafting run of `section-writer@1` for one
   section, an investigation), follows it and reads the staged result.
   The in-app agent does the drafting, on the runtime the user set for
   that role. An outside agent does not submit drafts or facts of its
   own; what it gets back is what the in-app agent produced, with its
   citations and review state. The grant names the outside client, the
   roles it may start and what it may read.
7. **Consent is per run and states what leaves the machine.** An SDK loop
   sends the provider what its tools return, so consent cannot be a
   preview of each request. Before a run the app states the role, the
   provider, the model and what the grant lets the agent read; the user
   starts it or does not. Every read the agent makes is recorded in the
   run, redacted as served. The existing loop keeps its consent per step.
   A task an outside agent starts needs the same consent from the human
   seat before a cloud runtime is used; with none, it runs on a local
   model or is refused.
8. **The host keeps the bounds.** Calls, bytes served and wall time are
   limited where the host serves the tools and by the run's deadline;
   when a limit is reached or the user cancels, the host stops the
   process. A run that ends without an admitted finish produces nothing.
9. **The existing loop stays.** It is the runtime for local models and
   remains available for the direct cloud lanes. The drafting acceptance
   (AC-0670) may be run on any runtime and records which.
10. **Order.** Starting a drafting run of a named role over MCP first,
    since it needs no new runtime; then the Claude Agent SDK runtime;
    then the OpenAI Agents SDK runtime.

## Consequences

- The team in the app can be staffed by local models, Claude agents and
  OpenAI agents under one roster, with one kind of reviewable output.
- The app gains a runtime to package, sign and update on each platform.
- An SDK run gives up the per-step payload preview. The run record of
  what was read takes its place, and the choice is stated before the run.
- What a model is told is bounded by what the tool surface serves, so the
  redaction and admission already built are the controls that matter.
- Codex and Claude Code get the team's work without holding a provider
  key of their own for it and without a path around review: they ask, the
  in-app agent drafts, a human accepts.
- Which role a task ran as, on which runtime and model, and which outside
  client asked for it are all in the run record.

## Not decided here

- Which language the sidecar is written in and how it is packaged.
- ACP hosting and the roster's coordinator (W8b).
- Default models for each runtime.
- Whether several agents of one team run at once.

## Alternatives rejected

- **SDK agents as external clients that submit drafts.** The earlier
  draft. It puts the team outside the product, uses MCP for the opposite
  of its purpose, and leaves the app unable to say which role produced a
  draft.
- **Reimplement the SDK loops in Rust on the direct APIs.** That is the
  existing loop; it is not the providers' agent SDKs and does not get
  their tool use, context handling or model support as they change.
- **Replace the existing loop with the SDKs.** Local models have no SDK
  runtime, and the consent per step of the direct lanes would be lost for
  users who want it.
