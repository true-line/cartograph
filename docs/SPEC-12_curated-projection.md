# SPEC-12 — The curated projection

- **Status:** Proposed for [#1746](https://github.com/true-line/cartograph/issues/1746); slices 1 (projection core, AC-0652), 2 (export, AC-0653), 3 (Workbench, AC-0655) and 4 (MCP, AC-0654) are implemented. MT-H3-01 has not been run.
- **Date:** 2026-11-12
- **Scope:** US-0021 / AC-0108; SPEC-01 H3; ROADMAP W5.
- **Decision:** [ADR-0061](adr/ADR-0061-curated-read-contract.md).
- **Extends:** [SPEC-03](SPEC-03_proposal-staging.md) (delivery boundary), [SPEC-09](SPEC-09_captured-task-basis.md) (basis assessment). ADR-0036 decisions 3 and 4 apply.

## Outcome and boundary

An accepted staged proposal whose evidence still holds appears, under one
curated revision, in the Spec Workbench, a `best-effort` export and an MCP
read. Every other accepted record is listed with the reason it does not
appear. Base facts, their tiers and the base graph hash do not change.

Not in this specification: a fact shape for investigation findings; any write
to the base graph; agent-authored document sections; re-drafting on drift;
MCP or ACP execution; extending captured evidence (#385).

## Terms

- **Record:** a staged proposal with a review (SPEC-03), or a Workbench
  assertion decision.
- **Slot:** the directed unresolved relation a proposal fills: the Gap node
  it cites, its source, and its edge label.
- **Curated item:** an eligible accepted proposal, shown beside recovered
  facts.
- **DR-4 fields:** the fields every curated read carries (below).

## The projection

`context_hub::curated::project(input) -> Result<CuratedProjection, _>` is a
pure function. Its input is the current graph facts and their content hash,
the reviewed proposals, the SPEC-09 basis assessment the host made for each,
the Workbench assertion decisions, the content hashes of the current
curatable assertions, and the policy version. Two records with one identity,
or two decisions for one content hash, fail the projection. The result
holds:

- `view`: `curated`.
- `revision`: the curated revision.
- `graph_basis`: the base graph content hash it was computed over.
- `items`: the eligible records, ordered by record identity.
- `listed`: every other accepted record with its state and reason, in the
  same order.
- `assertion_decisions`: the digest of the rejections applied, and the
  decisions that match no current assertion.
- `counts`: accepted, eligible, and each other state.

Records are read through bounded, deterministically ordered store reads. A
read that cannot finish within its bound fails; it never returns a partial
projection as complete.

### States

Each accepted proposal gets the first state in this order that applies:

| State | Condition |
|---|---|
| `unverified` | Not a version 2 record, a record with no evidence, or any evidence item that is not captured primary-source evidence |
| `already-covered` | The base graph holds the exact proposed edge (source, label, target) with valid provenance, a tier other than Agentic and a confidence other than Gap. The Gap node need not exist |
| `unresolvable` | The Gap, the source or the target is not in the current graph, or the node the record names as its Gap is no longer a Gap |
| `stale` | No assessment was supplied or it failed operationally; the SPEC-09 graph or association comparison is not `unchanged`; a captured item is not available or was not assessed; or the assessment counts unverified items the record does not |
| `conflicted` | Another accepted record names the same Gap with a different source, label or target, whatever that record's own state |
| `eligible` | None of the above |

Each state carries a reason code. An edge to a different target never covers
a record: with cardinality unproven, the graph cannot tie it to this Gap. Two
accepted records that propose the same edge for one Gap do not conflict.

The state is computed at projection time and is never stored. It is not a
freshness flag: the read reports the SPEC-09 dimensions it used beside the
state.

A proposal that is pending or rejected has no state and is not listed.

### Curated revision

`revision = digest("curated-revision-v1", policy_version, graph_content_hash,
[(record_id, review_decision, state, reason)…], assertion_decision_digest)`,
with the record list in record-identity order. A rejected record enters with
its decision and no state, so any review gives a new revision; a pending
record does not enter. The assertion-decision digest covers the rejections
applied and the decisions that match no current assertion. Review notes,
timestamps and review revision counters are excluded: they do not change
content.

### DR-4 fields

Per read: `view`, `revision`, `graph_basis`. Per item and per listed record:
`record_id`, `tier` and `confidence` as produced, `review_status`,
`source_support` (count of captured items and of unverified items), `state`
and `reason`.

## Surfaces

All three call the same function and report the same `revision` for the same
inputs.

- **Workbench.** A curated section lists items and listed records with their
  DR-4 fields, the proposed edge, the Gap and the basis dimensions. It is
  carried in the same response as the bundle (`export_spec_with_views`
  returns `curated` beside `bundle`), so both come from one projection. The
  records sit in a disclosure that is closed by default. The review dialog
  and proposal history state an accepted record's state and reason; in
  `verified-only` an eligible record is not returned, so they point to the
  section instead.
- **Export.** `SpecBundle` carries `curated_basis: {revision, counts}` beside
  `graph_basis`. A new artifact, `curated.md`, lists the items and the listed
  records. Each item cites its Gap and endpoints through the fact index
  (ADR-0058). `verified-only` contains no item and states how many it left
  out; `best-effort` includes them, marked InferredWeak. Existing artifacts
  are byte-identical when there is no accepted record. An item cites its Gap
  and endpoints only where the export admits those nodes.
  The host reads at most 4,096 staged records and 256 accepted ones, and
  assesses only accepted records that could be eligible. An assessment made
  against another store revision than the one the export reads counts as
  failed, so its record is listed as `stale`. When the projection cannot be
  computed (a bound is exceeded, the review stores are unavailable, or
  identities are duplicated) the export still succeeds: `curated_basis` is
  absent, `curated.md` states the reason, nothing from the projection is
  rendered, and every other artifact is unchanged. A current curatable
  assertion is a stored or compiled T2/T3 inferred assertion with evidence.
  Work packets refuse `curated`: the packet host gathers no reviews.
- **MCP.** A complete session reads the projection through the existing
  artifact read: `curated` serves the export's `curated.md` bytes for the
  same inputs and mode, and `curated-json` (`cartograph.mcp/curated-json@1`)
  serves the same projection as data. `ReadBasis` carries the curated
  revision, or the reason it was not computed, and it enters the basis
  identity. `h3_curated_available` is true only for a session whose
  projection was computed. Acquisition fails with `StaleBasis` when a review,
  a Workbench rejection or the graph changes while it runs; a session is
  frozen afterwards. Reviews are gathered only after the session's bounded
  graph read has succeeded. An agent read never compiles every document: when
  a Workbench decision matches no stored fact, the projection is not computed
  for that session and the reason says so, while an export still computes it.
  A scoped session has no complete graph and reports the read as unavailable.
  A `verified-only` session gets no item, only the count. Notes, timestamps,
  and pending or rejected records are not served.

## Slices

Each slice is one PR with its own acceptance criteria, reserved in that PR.

1. **Projection core.** `project`, the states, the revision, in a crate with
   no UI or MCP dependency. Tests: each state from a fixture; revision
   equality and inequality; determinism under input order.
2. **Export.** `curated_basis`, `curated.md`, both modes, fact-index
   citations. Replaces the test that asserts acceptance leaves the export
   unchanged.
3. **Workbench.** The curated section and the changed copy, with stories.
4. **MCP.** The curated read, the flag, `ReadBasis`, `StaleBasis`.

AC-0108 is realised when slices 1 to 4 are merged and MT-H3-01 has been run.

## Open items

- Findings (#1746 requirement 2) need a fact shape before they can enter.
- Cardinality is one accepted edge per Gap until slot cardinality can be
  proven from the graph.
- The Gap predicate is duplicated in `context-hub` and `spec`; a parity test
  in `spec` keeps them equal.
- A Workbench decision on a derived statement, or one that has gone stale,
  makes the curated read unavailable over MCP until the compile it needs is
  accounted for in the worker's limits (#1834).
- The fact index is not readable over MCP, so the citation ids `curated.md`
  prints cannot be resolved there.
- An export assesses each accepted record that could be eligible on the
  calling thread, and compiles the documents a second time when a Workbench
  decision matches no stored fact. Both costs are bounded and neither is
  moved off the calling thread yet.
- `ContextStatus::AwaitingReconciliation` stays on the wire and in stored
  records unchanged; no surface shows it any more.
- The MCP `user-stories` artifact and the desktop export are built from
  different inputs today (the knowledge overlay). Requirement 1 of #1746
  holds for `curated.md`, not for that artifact.
