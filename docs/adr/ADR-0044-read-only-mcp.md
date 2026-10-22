# ADR-0044 — App-owned read-only MCP with a separate local relay

- **Status:** Proposed — #718; owner review required
- **Date:** 2026-10-21
- **Deciders:** Cartograph owner; Codex implements, Claude cross-reviews shared semantics

## Context

External agents need qualified discovery, official artifacts and retained source
evidence without starting investigations. Desktop startup opens writable stores
and recovers work, so a stdio invocation of that executable is not a read-only
entry point. The app already owns projection policy and capture retention.

Initial W8a exposes recovered, qualified and rejection-filtered evidence, as
cross-reviewed in #719. Activated H3 curated context remains unavailable; accepted
annotations cannot substitute for the shared curated projection.

## Decision

1. Keep reads in an explicitly enabled service owned by the running desktop app.
   A separate console relay uses private authenticated local IPC; it never starts
   the app, opens its databases, recovers work, ingests or calls a provider.
2. Expose only implemented, typed read tools. Freeze and identify the raw graph,
   filtered projection, coverage, effective rejection policy and registration
   basis. Reuse compiler policy and bounded selected compilation. Flows are
   explicitly discoverable through the flow-dossiers artifact until a flow-query
   surface exists. No generic command, SQL, path or mutation dispatch is added.
3. Resolve retained evidence through the production version-aware receipt reader,
   validating original graph, receipt and exact range with retention protection.
   Missing, stale, forgotten, limited and explicitly receipt-omitted evidence stay
   distinct; no checkout fallback or synthetic citable receipt is permitted.
4. Authenticate before context acquisition. Activation is disabled by default and
   runtime-only; disable/restart invalidates credentials, connections and sessions.
   Same-account credential access remains an explicit trust boundary. Admit frames,
   work, retained sessions and caches independently; raw bytes do not measure RSS.
5. Deliver macOS/Unix first, with platform-specific acceptance. Windows is a later,
   separately verified deliverable, never an implicit fallback or Unix-test claim.
   Sign/package the relay for each advertised target. Provider execution remains
   separately authorized and is not needed to prove deterministic MCP reads.

Detailed schemas, limits, preamble, native transport construction and test contracts
live in [Read-only MCP implementation contract](../design/read-only-mcp.md).

## Consequences

Clients require the app running and explicitly enabled. Sessions expose their
limitations and can become stale. Official artifacts retain their actual content
identities; limited construction never fabricates one. H3 support waits for the
actual shared projection. Installed-client, lifecycle and platform security tests
remain necessary beyond protocol-unit tests. Claude agreed the shared projection
semantics and AC grouping; that cross-review is not owner approval or acceptance.

## Alternatives

1. **Standalone DB reader:** rejected; duplicates projection ownership and risks
   initialization/recovery effects and inconsistent store revisions.
2. **Desktop executable in stdio mode:** rejected; startup is not read-only and
   Windows GUI-subsystem packaging is unsuitable for a console relay.
3. **Loopback HTTP:** deferred; localhost is not authentication and introduces a
   network-facing surface unnecessary for the initial local bridge.
