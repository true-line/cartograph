# ADR-0066 — Cloud models: direct lanes in the app, agent SDKs as external clients over MCP

- **Status:** Proposed; owner review
- **Date:** 2026-11-17
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1745; #1748; #1763; ROADMAP §11 ("Providers"); ADR-0036; ADR-0057; ADR-0061; ADR-0064; AC-0670

## Context

The roadmap leaves one provider question open: whether the cloud lanes use
the providers' agent SDKs. It asks for an ADR before the drafting
acceptance of #1748 (AC-0670) is run on a cloud model.

What exists today:

- **Direct lanes, in process.** `crates/llm` calls a local Ollama server,
  the Anthropic Messages API (`AnthropicProvider`, three pinned lanes) and
  the OpenAI API (`OpenAiProvider`). Every cloud request goes through
  `EgressFirewall`: the user sees the exact payload of a step, consents to
  it, and the step is recorded. With no consent the lane is closed.
- **Keys.** A provider key is stored in the operating system's credential
  store and never in a file the app owns (ADR-0057, #1763).
- **The loop is Cartograph's.** An investigation, and a drafting run
  (`section-writer@1`), is a bounded loop the host drives. Each turn's
  input is admitted, each finding cites admitted facts, and a finish is
  staged only when the pure admission rule accepts it (ADR-0064).
- **An MCP surface.** `crates/mcp-local` serves read tools
  (`cartograph_discover`, `cartograph_read_fact`, `cartograph_artifacts`,
  `cartograph_read_artifact`, `cartograph_evidence`) and task tools
  (`cartograph_task_start`, `_status`, `_events`, `_result`, `_cancel`) to
  a client that holds a host grant.

What the agent SDKs are:

- The Claude Agent SDK and the OpenAI Agents SDK are Python and TypeScript
  packages. Neither has a Rust binding. Each runs its own agent loop in a
  process the user hosts, with the user's key, and each can use MCP servers
  as tools. The Claude Agent SDK also ships built-in file, shell and web
  tools. (To verify against the vendors' current documentation when the
  launchers are built.)

So "use the agent SDKs" cannot mean a library call from the Rust core. It
means either a Python or Node runtime inside the app, or an agent process
outside it.

## Decision

1. **The direct lanes stay the in-app cloud path.** Nothing in
   `crates/llm`, the egress firewall or the consent record changes. The
   drafting acceptance (AC-0670) may be run on a direct cloud lane; it does
   not wait for an SDK lane.
2. **An agent SDK is an external client, not an embedded runtime.** An
   agent built on the Claude Agent SDK or the OpenAI Agents SDK runs in the
   user's own process with the user's own key and reaches Cartograph only
   through the MCP surface, under a host grant. The app ships no Python or
   Node runtime in v1.
3. **Cartograph's guarantees hold at its own boundary.** The app cannot
   see or constrain how an external agent is configured, so nothing is
   assumed about it. What the MCP surface returns is already redacted and
   bounded, and what it accepts is propose-only (R-INT-3). An external
   agent never writes a confirmed fact and never changes one (R-INT-1).
4. **A draft from an external agent passes the same admission.** One new
   MCP tool accepts a section draft for a catalogue address: a list of
   claims with their kinds and citation ids. The host computes the
   citation set of the address from the current graph and applies the same
   pure rule a `section-writer@1` finish passes (ADR-0064 decisions 3 and
   4). An admitted draft is staged for review with its origin recorded as
   the external client; a refused one returns a closed reason code and
   stores nothing. Review, acceptance and staleness are unchanged.
5. **Consent for this lane is the grant.** Content an external agent reads
   through MCP leaves the machine in that agent's requests, not the app's.
   The user consents by issuing the grant to a named client; the grant is
   scoped, revocable and limited in identities (as today), and each read
   is in the MCP record. Per-step payload preview stays a property of the
   direct lanes, and the grant dialog says so in plain words.
6. **The provider key of this lane never enters the app.** The agent's
   process holds it. ADR-0057 applies only to the direct lanes.
7. **What the product ships for this lane** is small: the draft tool of
   decision 4, and for each SDK a short launcher and a Cartograph agent
   definition (role prompt, the MCP server entry, and no tool beyond the
   Cartograph MCP tools). Anthropic first, OpenAI second.

## Consequences

- No new runtime, signing or update surface in the desktop app, and no
  second agent loop to keep bounded.
- The agent team of the vision (humans and agents working through MCP and
  ACP) and the SDK lane are one mechanism, not two.
- An SDK agent's draft is held to the same citation rule as a local one,
  so a reviewer sees one kind of draft whatever produced it.
- The user gives up the per-step payload preview on this lane. A reader
  who needs it uses a direct lane.
- A model choice inside an SDK agent is the user's; the app records the
  client id and what the client declares, not a verified model identity.

## Not decided here

- ACP hosting and the in-app roster (W8b).
- Whether a later release embeds a sidecar runtime so an SDK agent can be
  started from inside the app.
- Which models the launchers default to.

## Alternatives rejected

- **Bundle a Node or Python sidecar and run the SDK loops inside the app.**
  It adds a runtime to sign, update and sandbox on every platform, and the
  SDK loop sends what its tools return, so per-step consent would not hold
  there either. It buys an in-app start button and costs weeks before v1.
- **Replace the direct lanes with the SDKs.** The bounded loop, the
  admission of each turn and the egress record are what make a finding
  citable; an SDK loop the app does not drive cannot give them.
- **No SDK lane in v1.** Simplest, and it leaves the stated direction of
  running Anthropic and OpenAI agents through their SDKs unserved while
  the MCP surface that would serve it already exists.
