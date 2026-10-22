# Scoped MCP retrieval

Status: implementation draft for #731. Depends on the storage revision contract
in merged #744/#750 and read-only MCP in #718/#727. This document does not establish a shipped
capability, native acceptance, owner approval, or completed issue gate.

## Acquisition and disclosure

Human activation explicitly selects `acquisition_mode`: `complete` or `scoped`.
Omitting the field preserves the existing complete-session behavior. A failed
complete acquisition never falls back to a scoped one. Numeric graph, coverage,
request, response and retained-source limits remain explicit, with unchanged hard
maxima. Scoped mode interprets graph limits as a bounded acquisition window or
selected fact closure, rather than a complete graph allowance.

The first scoped acquisition records coverage and a storage revision without
loading all facts. Discovery/detail freshness checks bracket their graph reads
with registration, configuration, rejection and revision observations. A changed
observation returns `stale_basis`; clients open a new session. Partition requests
instead pin the expected graph revision for their complete storage acquisition,
as described below. Checks of external qualifications are not a transaction
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

An unavailable receipt or capture is classified as forgotten only when the
retention ledger proves it for the exact current binding under that guard.
Supplied receipt, digest and source identities are checked before this lookup;
missing bindings do not establish forgetting. Invalid metadata and operational
failures keep their distinct errors. The scoped capability advertises this
partial forgotten classification separately from receipt omission.

For an unbound current fact, describe-only evidence requests can return
`evidence_omitted` with structured `omission_kind`: `declarations`, `rules` or
`store_capacity`. The indexed producer omission, complete fact digest and store
revision are selected in one bounded transaction, then the exact selection and
registered source are rechecked under the retention guard before session
qualifications are rechecked. This result returns no target or span.
`NotOmitted` and `Unrecorded` remain unavailable; missing bindings alone prove
neither omission nor forgetting. A supplied evidence target that loses its
binding is stale, and a current binding and its exact forgotten classification
take precedence over omission metadata. The separate partial
`receipt_omission_classification` capability advertises this support; broad
`evidence_absence_reason_available` remains false.

Scoped `cartograph_read_artifact` supports three producer-qualified partition
kinds: `data-model`, `rule-evidence` and `rule-evidence-index`. The catalog names
those bounded scopes separately from complete-only artifacts. `flow-dossiers`
remains unavailable: there is no explicit-seed tracer closure API in this slice.
Complete acquisition keeps its existing compiler, schemas and limits unchanged.
A union of discovery pages is never a producer closure or complete artifact basis.

### Closed stored-fact partitions

Only the storage-owned scoped reader constructs the private closure, inside one
deferred SQLite transaction checked against the expected StoreRevision. It uses
validated indexed adjacency, charging all inspected labels before deciding which
ones the producer needs. Cumulative visits include duplicates/missing probes;
raw/canonical bytes, JSON values/depth and bounded adjacency probes also consume
the existing graph budget. A small entry can reach a large component and fail
the whole request. Missing required context or exhausted acquisition/output
limits produce unavailable/limited, with no prefix artifact or content identity.

A data partition exhausts both directions of the native data-model relation set
(`READS`, `WRITES`, `MAPS_TO`, `REFERENCES`) for its projected visible DataEntity
component, and includes required incident context endpoints. A Gap-tier DataEntity
entry root returns unavailable by design; this is not a claim that the fact or
its source data is missing. Hidden, rejected or
below-mode bridges are inspected and charged at the visible boundary, but never
expanded. High-degree boundary rows can still exhaust the
acquisition limit; hidden components cannot join visible roots or render as
part of their component. A rule partition renders one
explicit BusinessRule observation plus its outgoing native inventory relations;
its owner, dependency targets, unresolved/interpretation gaps and local-definition
references are acquired as rendering context. Context observations never become
extra inventory roots. Lexical binding IDs are not graph-node requests. Existing
native visibility, tier/provenance handling, gap rendering and malformed-payload
limitations remain in force; stored closure is not a proof of complete execution
predicates, consumer effects, source truth or behavioral completeness.

The compiler shares official projection/rendering, including hidden BusinessRule
relationship policy, but returns `cartograph.spec-partition/<kind>@1`,
`spec-partition-basis-v1` and `spec-partition-artifact-v1` identities. The basis
binds sorted projected component/context facts, semantic roots, artifact kind, StoreRevision and
qualified scoped session/export mode. The requested data entry is response
metadata; two visible entries in the same component produce identical canonical
content/basis/content IDs. Acquisition counters are disclosed outside that
identity. Rule roots are explicit observation selections. Every response carries
`complete_system: false` and `absence_claim: false`; no complete context ID or
whole-system recovery claim is substituted.

A concurrent graph writer can commit while the partition transaction retains its
original snapshot. A successful response still names that pinned revision; it
does not assert the store stayed unchanged until delivery. The next request
using the older revision is stale. Registration/configuration and rejection
qualifications are independently checked before and after the graph transaction;
that is not a cross-store transaction or an exclusion of ABA changes.

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
Omission checks include two selected fact/omission comparison owners and bounded
SQL cell decoding. Source-v2 omission objects use the shared exact source,
coverage and generation schema workspace; primary-key preflight scratch remains
separate. These additions do not raise policy ceilings.
Partition work adds closure maps/current indexed pages, frontier and typed rule
reference sets before graph lookup, projection/native rendering, qualified
header/body/assertion owners and serialized protocol Values while the frozen
session remains retained.
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
complete artifact unavailability, and producer-qualified partition identity,
dangling required context, whole-component limits and whole-envelope refusal.
Measure actual latency and resource
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

## Partition validation scope

AC-0311/T-0311 covers pinned indexed adjacency, component identity and native
producer parity, required-context/limit refusal and partition workspace roles.
AC-0312/T-0312 maps the scoped dispatcher artifact route on a large synthetic
estate, exact serialized envelope boundary, root-independent identity and stale
refusal, the authenticated native Unix socket/worker partition path, and a rule
partition assertion followed through the production retained-source dispatcher
path after checkout removal. These are automated fixtures, not installed-client
or provider acceptance. The earlier timing observation above retains its
original discovery/detail scope: its unavailable-artifact observation predates
this partition phase. Positive retained-evidence transport latency and the full
evidence denominator remain separate from partition rendering or a source-backed
dispatcher assertion. AC-0288..0290 and the full installed/native/independent-client
manual gates are unchanged; no complete #731 delivery is claimed here.

## Independent scripted client fixture

The opt-in
`scoped_scripted_client_partition_and_retained_span_after_checkout_removal`
fixture starts the production authenticated service in explicit scoped mode. A
separate Node process drives a separately source-built relay over stdio, selects
a rule partition, follows its assertion to an exact retained descriptor, and
reads the captured span after the synthetic checkout has been removed. The host
compares the returned bytes by length and SHA-256 with the original captured
source and checks that the graph is unchanged. The client also checks stale
session identity, response-limit refusal, clean relay EOF and service cleanup.

Run only with an explicit absolute `CARTOGRAPH_TEST_MCP_RELAY` path and the exact
ignored test selected. Record the source tree/diff and relay/test binary hashes
with the result. Fixture limits are explicit test inputs; they are not new
product policy defaults. This fixture is independent-process protocol evidence,
not installed-client, native UI, production-signing or provider acceptance. It
does not replace the separately attributed large-graph measurements or establish
positive-evidence latency guarantees.

Observed on 2026-10-22: the exact opt-in test passed once with zero failures.
It returned 99 retained bytes with SHA-256
`96302f89516cd78c7274affc984dc7bba27b6981ace5ad16d6abe8608182e2f7`;
both refusal controls passed, relay exit was zero with no stderr, the activation
record was removed, and the graph remained unchanged. The executed source was
signed base `0552319b8ecdb8c3dcf5043558607c28e8d19774` plus the test/script
changes in this update; it was not a clean signed-head run. Source inventories
before and after execution matched. The app-test binary SHA-256 was
`2b48c7c597788eb9ad5aa8f7abbdf0fe185913c19438adfc5ded383bccd06baa`; the debug relay SHA-256 was
`84d8a9d9bf2d02a445f6de0d510acd9ac5fd79fba0faa98b7f81ddba99613ae2`. Raw result, source inventories and binary hashes are retained
in `/tmp/cartograph-codex/755-scoped-client/` on the executing host. The fixture
and independent client are committed so the observation can be reproduced;
the local scratch directory is not a portable evidence archive.
