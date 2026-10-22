# Atomic graph generation (#744)

Status: proposed. Editor: Codex. Prerequisite to #731; shared with #725.
Decision: [ADR-0047 — Graph store revision](../adr/ADR-0047-graph-store-revision.md); acceptance: AC-0302/T-0302 and AC-0303/T-0303 (US-0020).
The storage implementation is under review. Scoped agent acquisition and
completed-recovery integration remain separate, unshipped work.

## Contract

A graph generation identifies a committed store revision together with the
existing graph instance. It is independent of recovered graph content hashes,
fact identities, confidence and provenance. Generation advances through guarded row triggers in the same SQLite transaction
as the affected facts, source associations and coverage. It is an opaque
monotonic token, not an operation count; one transaction can advance it many
times. Readers obtain generation and requested data
inside one read transaction. A continuation from another generation is stale.

Generation is not a recovery status, capture timestamp or receipt identity.
An unrelated later graph mutation cannot establish capture supersession. A
completed-recovery record must bind its source, operation and exact capture to
the successfully published graph revision. Cross-store publication remains a
separate crash-consistency problem for #725.

## Additive storage migration

Introduce a privately versioned singleton table with version 1 and an integer
generation in the inclusive range 0 through SQLite's maximum signed integer.
Validate the owned schema, singleton cardinality, version and integer type.
Malformed, partial or future-version metadata is an error, never a reason to
reset the counter. A fresh database and an intact legacy database without this
metadata start at generation zero. Adding it preserves existing facts, source
bindings, coverage and graph instance; do not change GRAPH_SCHEMA_VERSION to
perform this migration, because that version controls destructive fact-ID
rebuilds.

Initialize metadata under an immediate transaction. Existing fact-schema
resets must atomically clear facts and coverage, advance generation and update
user_version. The external derived-state invalidation hook runs before graph
deletion; its effects are not part of SQLite rollback. Document and test the
ordering, including hook failure.

The migration and ownership schema need explicit review before publication.
Row triggers cover already-open older writers and direct SQL data mutations
while the owned schema remains intact. They do not protect against a writer
that deliberately disables or rewrites the schema. Fresh older application
versions may reject the extended exact schema; do not claim downgrade support.
Restored database files retain instance identity and may roll generation back;
file-level restores are not detected. Continuation across a restore is unsupported.

## Mutation boundary

Watch INSERT, UPDATE and DELETE on nodes, edges, source_node_bindings,
source_edge_bindings and coverage_inventories. Each trigger checks exhaustion
and advances the counter. Source-binding invalidation and foreign-key cascades
therefore participate automatically. Failed statements and rolled-back
transactions restore both rows and generation. A fact-schema reset also
advances generation explicitly within its reset transaction, including an empty
reset; ordinary absent deletes and empty clears need not advance it.

An identical UPDATE may advance generation. One batch publication can advance
many times, including binding-only and coverage-only changes. No caller may
interpret a difference of one as one recovery or infer elapsed time from it.
At the maximum signed integer, fail the mutation rather than wrapping,
converting to REAL or resetting.

Coordinate exact recognized SQL with the existing source and coverage schema
validators. They currently inspect every trigger on their owned tables.
Recognize only the complete expected generation schema and SQL; never exempt
an arbitrary name prefix. A partial generation schema must fail closed before
any migration repair or graph deletion. New and legacy initialization install
the complete table/trigger set atomically after the base tables are available.

Alternatives considered: public-method transaction bumps are cheaper but miss
already-open older writers unless an exclusive writer/version gate is added.
Long-lived read transactions retain snapshots but introduce writer/WAL lifetime
costs across client calls. The selected trigger approach preserves short read
transactions and storage-level invalidation.

The actual Rust bulk-publication API was measured on synthetic chains in an
external harness, with three interleaved old/new repetitions per size and a
fresh production-opened database for every sample. Median API wall times were
282.422 / 428.776 ms (old / new) for 10,001 rows and 2,189.196 / 3,375.533 ms
for 60,001 rows. All 12 publications and readbacks succeeded; new generations
matched the inserted row counts. Checkpointed database size increased by
8,192 bytes at both sizes.

The harness used Rust 1.96.1, unoptimized dev builds with debug info and
incremental compilation disabled. The old source was `ebea61aefc1bd462b0862815ffc6483cc1ef5e6a`;
the new source was the recorded dirty core-graph tree atop
`b99fc07f2c54f7e127ce70663f5dbcffd60490a5`. Source hashes were stable over the
measurement; subsequent validation-guard changes are outside that measured
tree. The external report and source manifests are in
`/private/tmp/cartograph-codex/744-rust-measure/`. Shared-machine timing,
uncontrolled caches and three samples do not establish a causal slowdown,
production throughput or an RSS bound. The observed overhead is a tradeoff,
not justification to weaken consistency.

## Read boundary

Expose a typed store revision, separate from complete or scoped content IDs.
The helper that reads it accepts the current SQLite read transaction; callers
must not combine an autocommit revision lookup with a later data read. Provide
a coherent bounded snapshot seam to exercise the contract before #731 adds
scan windows and selected evidence reads. No transaction remains open across
external client calls.

Schema validation must check metadata sizes before copying SQL into Rust.
The generation objects require bounded metadata workspace in addition to the
requested graph data. Consumers in #727/#731 must account for that workspace
when integrating these APIs; existing transport reservations do not by
themselves establish an allocation or RSS bound for this new validation path.

For #731, compare the expected revision before reading the next window within
that window's transaction. Return a typed changed-basis failure instead of
mixing pages. The generation does not replace projection, rejection-policy,
registration, coverage or receipt qualifications.

## Required evidence

- Each writer advances the counter with actual committed row events, including
  binding-only and coverage-only changes and an already-open second connection.
- Identical updates and absent deletes follow the explicit event policy above.
- Stale patches and injected failures preserve the old revision and rows.
- Exhaustion refuses mutation without wraparound, REAL conversion or reset.
- Reopen preserves revision; different stores retain distinct instances.
- Additive legacy migration preserves facts and bindings; malformed and future
  metadata fail closed; destructive fact-schema reset is atomic.
- A contended opener reads the schema version after acquiring the writer lock;
  this exercises contention, not an instrumented reproduction of the former
  read-to-lock race window.
- A competing connection commits between reader phases: the result contains
  one coherent revision and its data, while a later continuation is stale.
- Interrupted recovery never manufactures a completed-capture signal.

The existing #731 synthetic harness supplies a before-change acquisition
baseline. It is not evidence of generation correctness, memory bounds or the
performance of a future scoped implementation.

## Release publication comparison (PR #750 review)

An optimized release build compared published `d3d13c936a6b635682f660780775328c435cd872`
against baseline `f79bbda285d90cc70293ca5a422525512d5bb10c` using three fresh-database
process runs per version and size. Production release settings included thin LTO,
one codegen unit and abort-on-panic. Each timed call published an empty-to-chain
GraphPatch and validated readback; setup and readback were outside the timed call.

| Combined node/edge rows | Baseline median | Generation median | Ratio |
| --- | ---: | ---: | ---: |
| 60,001 | 965.817 ms | 1,389.360 ms | 1.4385× (+43.85%) |
| 250,001 | 4,039.043 ms | 5,884.529 ms | 1.4569× (+45.69%) |

All 12 publication and readback assertions passed. The release result confirms a
material publication-time tradeoff for the owner to review. It does not isolate
trigger cost from all implementation differences, measure typed source-binding
or coverage publication, predict full application ingestion time, or establish
RSS bounds. Runs were interleaved on a shared machine with warm caches, not a
controlled cold-cache performance experiment. Exact source archives, release
binaries, locks, compiler settings and pre/post hashes are retained with the
external task artifact `750-release-measure/REPORT.md` and raw results. The later
metadata-count cleanup is outside this measured head and does not change the
publication trigger mechanism.
