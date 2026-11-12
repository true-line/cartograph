# SPEC-12 — The curated projection

- **Status:** Proposed for [#1746](https://github.com/true-line/cartograph/issues/1746); no slice is implemented.
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

`project(graph, proposals, assertion_decisions, policy) -> CuratedProjection`
is a pure host function. It holds:

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
| `unverified` | Version 1 record, or any evidence item that is not captured primary-source evidence |
| `unresolvable` | The Gap, the source or the target is not in the current graph |
| `stale` | SPEC-09 graph or association comparison is not `unchanged`, a captured item is not available, or the assessment failed operationally |
| `already-covered` | A base fact has the same source, edge label and Gap slot |
| `conflicted` | Another accepted record names the same Gap with a different target |
| `eligible` | None of the above |

The state is computed at projection time and is never stored. It is not a
freshness flag: the read reports the SPEC-09 dimensions it used beside the
state.

A proposal that is pending or rejected has no state and is not listed.

### Curated revision

`revision = digest("curated-revision-v1", policy_version, graph_content_hash,
[(record_id, review_decision, state)…], assertion_decision_digest)`, with the
record list in record-identity order. Review notes, timestamps and review
revision counters are excluded: they do not change content.

### DR-4 fields

Per read: `view`, `revision`, `graph_basis`. Per item and per listed record:
`record_id`, `tier` and `confidence` as produced, `review_status`,
`source_support` (count of captured items and of unverified items), `state`
and `reason`.

## Surfaces

All three call the same function and report the same `revision` for the same
inputs.

- **Workbench.** A curated section lists items and listed records with their
  DR-4 fields. The accept dialog and proposal history state the record's
  state instead of "does not yet change context or exports".
- **Export.** `SpecBundle` carries `curated_basis: {revision, counts}` beside
  `graph_basis`. A new artifact, `curated.md`, lists the items and the listed
  records. Each item cites its Gap and endpoints through the fact index
  (ADR-0058). `verified-only` contains no item and states how many it left
  out; `best-effort` includes them, marked InferredWeak. Existing artifacts
  are byte-identical when there is no accepted record.
- **MCP.** A curated read returns the projection. `h3_curated_available` is
  true only in the capabilities and basis of a build where that read exists
  and computed the projection. `ReadBasis` gains the curated revision, and a
  read fails with `StaleBasis` when the revision changes during the read.

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
- Cardinality is one accepted record per Gap until slot cardinality can be
  proven from the graph.
- The MCP `user-stories` artifact and the desktop export are built from
  different inputs today (the knowledge overlay). Requirement 1 of #1746
  holds for `curated.md`, not for that artifact.
