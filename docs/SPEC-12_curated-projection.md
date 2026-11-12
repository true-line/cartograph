# SPEC-12 — The curated projection

- **Status:** Proposed for [#1746](https://github.com/true-line/cartograph/issues/1746); slice 1 (projection core) is implemented, AC-0652.
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
- Cardinality is one accepted edge per Gap until slot cardinality can be
  proven from the graph.
- The Gap predicate is duplicated in `context-hub` and `spec`. Slice 2 adds a
  parity test or moves it to a shared crate.
- `ContextStatus::AwaitingReconciliation` and the doc comment on
  `ProposalDecision::Accepted` in `agents` still describe the model before
  ADR-0061. They change with the slice that first shows a state to the user.
- The MCP `user-stories` artifact and the desktop export are built from
  different inputs today (the knowledge overlay). Requirement 1 of #1746
  holds for `curated.md`, not for that artifact.
