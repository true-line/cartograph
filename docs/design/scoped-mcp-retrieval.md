# Scoped MCP retrieval

Status: implementation draft for #731. Depends on the storage revision contract
in #744 and read-only MCP in #718. This document does not establish a shipped
capability, native acceptance, owner approval, or completed issue gate.

## Acquisition and disclosure

Human activation explicitly selects `acquisition_mode`: `complete` or `scoped`.
Omitting the field preserves the existing complete-session behavior. A failed
complete acquisition never falls back to a scoped one. Numeric graph, coverage,
request, response and retained-source limits remain explicit, with unchanged hard
maxima. Scoped mode interprets graph limits as a bounded acquisition window or
selected fact closure, rather than a complete graph allowance.

The first scoped acquisition records coverage and a storage revision without
loading all facts. Each request brackets graph reads with registration,
configuration and rejection observations. A changed observation returns
`stale_basis`; clients open a new session. These checks are not a transaction
across independent stores and do not exclude ABA changes in those observations.
Storage generation denotes mutation order, not successful recovery completion.

Discovery scans primary-key windows and applies the shared deterministic matcher
and official recovered-fact projection. It returns lean fact handles, source and
confidence summaries, match reasons, loaded/scanned counts, and a continuation.
Selected detail reads supply full projected fact properties. Discovery does not
invent node degrees: unavailable degrees are null. Edge detail reads its endpoints
at the same revision so a missing endpoint cannot bypass projection rules.

`session_basis_id` belongs to `mcp-scoped-session-v1`; discovery's query basis
belongs to `scoped-read-basis-v1`. Neither is a complete `context-v1` identity.
The cursor binds the query basis and a caller-selected raw `after_id` lower bound.
It is not an authenticated record that earlier keys were visited. `Exhausted`
means only that the requested tail has no more raw rows at that revision. Empty
matches, missing selected keys, and exhausted tails never establish global
absence, complete recovery, or why evidence disappeared. Outputs carry
`complete_system: false` and unknown remaining counts.

## Evidence and artifacts

Scoped evidence has a separate `mcp-scoped-evidence-target-v1` descriptor. It binds
the session basis, revision, selected fact digest, registered source, receipt,
receipt inventory ordinal, role, source reference and captured span. It cannot be
used as an existing complete-snapshot inspection target. Before reading bytes the
host revalidates selected identity and source binding, acquires the retention
guard, repeats the checks, and rechecks session qualifications. It uses the
existing retained receipt/capture reader, never a checkout fallback.

All current official artifact producers require the complete producer basis in
this slice. Scoped artifact catalog entries explicitly report unavailable with
`complete_basis_required`; scoped sessions do not advertise artifact compilation.
A sequence of discovery pages does not become an official artifact basis.
Partition artifact support requires a separate producer-qualified closure:
endpoint/entity dependencies, all required flow hops and alternatives, or full
rule/condition/dependency support at one revision. Over-limit closure must return
limited/unavailable without minting an artifact content identity. That work
remains part of the #731 plan, not a capability implied by this first slice.

## Alternatives and bounds

Increasing the complete graph ceiling retains the all-graph allocation and does
not solve narrow access on an enterprise estate. Arbitrarily truncating a graph
would corrupt completeness and compiler identity. Prepartitioning by inferred
repo/path would omit unbound facts and make partition ownership appear stronger
than the available evidence. Primary-key windows avoid those claims and retain
stable continuation using the authoritative mutation revision. Additional
indexed scope predicates need an explicit ownership contract and measurements.

The scoped worker reservation accounts for simultaneous retained and transient
owners: revision/schema validation, coverage and policy observations, raw and
projected rows, candidate handles, selected endpoint closure, binding/digest
scratch, receipt/capture buffers, protocol descriptors and response envelopes.
Checked arithmetic refuses overflow and a reservation below the derived minimum.
The transport reservation remains separate. These named payload units are not an
RSS guarantee; allocator overhead, SQLite caches and native runtime memory are
not measured by the logical ledger.

## Required validation

Keep complete-mode output and rejection behavior unchanged. Exercise a synthetic
60,001-row graph through the actual MCP worker, including initialization,
discovery and selected detail, then retained evidence after checkout removal.
Cover mutation between pages, rejection/configuration drift without graph change,
hidden rule endpoints, zero-match partial windows, invalid targets, output caps,
and complete artifact unavailability. Measure actual latency and resource
observations with a reproducible fixture and exact source tree. A real target
needs separate authorization. Full local gates, current-head CI and required
owner review remain publication gates; source fixture tests do not establish
installed-app v0.19 or native release acceptance.

## Synthetic native transport observation (2026-10-22)

The ignored `scoped_mcp_synthetic_measurement` fixture uses 60,001 nodes and
60,000 edges, real authenticated Unix sockets and the production worker and
scoped dispatcher. Three sessions traversed all 60,001 node rows in 15 windows
each, preserving basis/detail identities. Complete acquisition refused the same
120,001-row graph. Initializations took 1.1–4.8 ms; full node discovery took
1.428–1.468 s; six selected node/edge reads had a 3.285 ms median. These debug
profile, shared-machine observations include client framing and use warm shared
process state; they are not latency guarantees or release benchmarks.

A separate SQLite writer completed in 0.493 ms and the subsequent cursor read
refused the stale basis. WAL file length remained 9,809,752 bytes; this is not a
retention or long-running contention bound. Process RSS was 92,320 KiB at fixture
readiness, with a sampled session peak of 98,160 KiB and OS whole-process highwater
100,777,984 bytes. Harness, allocator and fixture memory are included. Logical
reservation was 174,196,282,675 payload units under the fixture policy, deliberately
reported separately from process memory; no practical deployment ceiling follows
from this test.

The exact Cargo artifact, frozen source manifests and binary hashes were checked
before and after the run. This proves synthetic native transport behavior, not
installed-app acceptance, positive retained evidence latency, partition artifact
content, independent clients or providers. The fixture reports unavailable
evidence/artifacts explicitly. Reproducible build/runner scripts and captured
results are retained in the external task artifact `731-mcp-measure/run-final`;
source checkpoint was `6887125` plus the ignored measurement fixture.
