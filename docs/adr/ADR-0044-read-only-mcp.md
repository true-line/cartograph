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

## Addendum — Explicit bounded acquisition (#731)

Storage revision is a prerequisite: ADR-0047 (graph store revision, #744)
provides an opaque mutation-order token, not recovery completion or a complete
graph identity. This branch depends on that storage implementation; it does not
redefine its private schema or migration contract.

Preserve complete acquisition and official artifact compilation; add a
human-selected scoped mode with no automatic fallback. Omitted acquisition mode
retains complete mode. Scoped reads use bounded indexed primary-key windows and
exact selected closure at an expected StoreRevision, with independently observed
registration/rejection/coverage qualifications. Bases and targets are
domain-separated from complete context IDs. Lean discovery handles lead to
selected fact detail and retained primary-source descriptors/text. Compiler
visibility is reused exactly, including endpoint closure for hidden BusinessRule
relationships; no broader endpoint-filter policy is introduced. Degrees and
global absence remain unknown. A caller-selected lower key is positioning, not
an authenticated issuance claim; Exhausted describes only the raw tail after
that key. Qualifier or storage revision changes invalidate continuation.

Authenticate and admit the mode-specific checked logical worker floor before
acquisition; transport stays independently activation-reserved. Keep named
schema metadata, fixed identities, current-window/selected-closure, digest and
binding work separate from graph canonical bytes. A running worker retains its
permit after waiter cancellation; evidence-validation charges remain
nonrefundable. Numeric ceilings remain unchanged and logical units do not
constitute an allocator or RSS guarantee.

Acceptance is AC-0304/T-0304 through AC-0308/T-0308 under US-0041, with explicit policy
interaction AC-0309/T-0309. Automated native Unix socket fixtures establish
the bounded app-owned path and explicit complete-mode refusal on a synthetic
over-limit graph; they do not establish installed packaging or independent-client
acceptance. Existing AC-0288..0290 and their manual acceptance denominator remain
intact.

### Closed stored-fact partition phase

AC-0311/T-0311 and AC-0312/T-0312 add producer-specific partitions without
changing the complete compiler or its admission ceiling. Only a private closure
built through a pinned, bounded storage reader is accepted. Data partitions
exhaust the projected visible DataEntity component for the native relation set and
include required context endpoints. Rule partitions select one explicit
BusinessRule observation with its owner/dependency/gap context; context rules
remain lookup-only. Native projection/renderers, tier/provenance and hidden
BusinessRule relationship policy are shared. Over-limit or missing required
closure refuses the artifact, never a truncated prefix or an empty-complete view.
Hidden/rejected/below-mode bridges consume boundary acquisition charges without
expanding hidden reach or joining visible roots. Boundary degree can still
exhaust the acquisition limit. Flows remain unavailable pending a separate
explicit-seed tracer closure.

Partition schema/basis/content domains are separate from complete artifacts and
context-v1. Sorted projected component/context facts and semantic roots bind the expected StoreRevision,
artifact kind and qualified session. The requested data entry appears only in
the response envelope, so two visible entries in one component share canonical
content and identity; acquisition counters do not affect that identity. Stored
closure never establishes behavioral completeness or full-system coverage.

One SQLite snapshot covers the whole partition acquisition. Concurrent graph
commits cannot alter that result's named revision; later requests using the old
revision fail. External registration/configuration and rejection observations
are checked before/after separately, not atomically with the graph or against
ABA changes. The worker floor additionally accounts for closure/frontier/typed
reference ownership, native rendering and qualified protocol output while the
session remains live; transport and evidence limits retain their prior gates.

Automated storage/spec, large-estate dispatcher and authenticated native worker
partition fixtures, plus the source-backed rule-assertion dispatcher fixture,
remain distinct from installed acceptance. The synthetic authenticated Unix measurement described in the
[scoped design](../design/scoped-mcp-retrieval.md) records discovery/detail at its
original pre-partition source; it does not establish partition transport timing,
positive retained-evidence latency or installed acceptance. H3 curated projection,
installed/native packaging, independent-client/provider acceptance and Windows
support remain pending under their prior gates. AC-0288..0290 and their manual
evidence denominator are unchanged. Neither this addendum nor the partition
fixture claims complete #731 delivery or owner approval.
