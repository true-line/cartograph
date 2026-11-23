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

Not in this specification: any write
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

`revision = digest("curated-revision-v4", policy_version, graph_content_hash,
[(record_id, review_decision, state, reason)…], [(draft_id, review_decision,
state, reason)…], [(finding_ref, review_decision, state, reason)…],
[(attested_id, state, reason)…], assertion_decision_digest)`, with the
record, draft, finding and attested lists each in identity order. Version 2
added the section drafts of SPEC-15 (ADR-0064 decision 7). Version 3 added
findings accepted as curated (ADR-0074 decision 6). Version 4 adds attested
records (ADR-0090 decision 8); the policy version is `curated-policy-v4`. A rejected record enters with
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

## Attested context

[ADR-0090](adr/ADR-0090-attested-context.md) adds attested context as the
fourth record kind. The projection is built (AC-1075,
`context_hub::curated::attested`), and the export and the MCP read take
every stored record into it. `curated-json` carries `attested` (the
eligible and the agent-attested, unconfirmed ones with their label and
contributors, in `best-effort` only), `attested_omitted`, `attested_listed`
(every other record, without its statement) and `attested_counts`. The
revision covers each record's attestations, since a read prints them.
Printing in documents and capture are not built yet.
Every stored record is projected: its attestations are its review. An
attested record gets the first state that applies: `withdrawn`, `disputed`, `already-covered`,
`unresolvable`, `stale`, then `eligible` when an engineer stated or
confirmed it, otherwise `agent-attested, unconfirmed` (agents only). The
last is never promoted to accepted without an engineer. `already-covered`
means the answered Gap kept its id and is now a node an ingest recovered
deterministically (valid provenance, not Agentic, not Gap confidence); a
Gap id with no node, or now an Agentic node, is `unresolvable`, as is a
supporting `F-` id with no current hash. A section target names no graph
node and is resolved by its supporting citations alone. The revision and the policy move to version 4. An attested record
never changes a base fact or the Gap count, and its DR-4 `tier` and
`confidence` are empty: it is labelled *attested* with its contributors.

## Findings

[ADR-0074](adr/ADR-0074-curated-findings.md) makes an investigation finding
the third record kind. It is built in three slices: the projection core
(AC-0843, implemented), the curation review (AC-0844, implemented), the
reads (AC-0845, implemented) and the screens (AC-0846, implemented).

- **Two reviews.** The follow-up review of SPEC-03 is unchanged and is not
  curation. A separate curation review, `accepted` or `rejected`, is what
  the projection reads. A finding with no curation review is not listed and
  does not enter the revision.
- **Input.** Per finding: its complete identity (the SPEC-03 review
  subject id), the investigation, result and finding ids, the producing
  role, the claim kind, title, statement and limitations, and for each
  cited fact its ADR-0058 citation id with the fact digest the producing
  run recorded. The host also passes the current digest of each cited fact
  that is in the graph.
- **States.** Each accepted finding gets the first that applies:

  | State | Condition |
  |---|---|
  | `unresolvable` | It cites no fact, or a cited id has no current digest |
  | `stale` | A cited fact's current digest differs from the recorded one |
  | `eligible` | Neither |

  Findings do not conflict and do not supersede one another. The changed
  and the missing ids are reported whatever the state.
- **DR-4 fields.** `finding_ref`, `tier` Agentic and `confidence`
  InferredWeak, `review_status`, `source_support` (citations that rest on
  captured primary-source evidence, and those that do not), `state` and
  `reason`. Source support is reported and does not decide the state.
- **Result.** `findings` lists every accepted finding in identity order and
  `finding_counts` totals them by state. Two findings with one identity
  fail the projection.
- **Curation review (as built).** `curate_investigation_finding(requestJson)`
  takes `{schema_version: 1, subject, expected_revision, decision}` with
  `decision` `accepted` or `rejected`, and
  `get_investigation_finding_curations(investigationId, expectedResultId)`
  returns the current review of each reviewed finding of a result. The
  host resolves the finding from the saved result under the investigation
  store's lock, releases it, and then writes under the section draft
  store's lock: the curation table `curated_findings` lives in that store's
  file, so the read that gathers drafts gathers findings under the same
  lock. The first review stores the finding with one entry per cited fact;
  a trigger refuses any change to that content and any delete. A failure
  has one closed code: `invalid_request`, `invalid_record`, `not_found`,
  `subject_changed`, `conflict` with the current revision, `busy` or
  `storage_unavailable`. A committed review emits
  `investigation-curation://changed`. The host reads at most 4,096
  reviewed findings, and accepted ones up to 16 MiB of encoded payload
  (256 findings of the largest size the store admits; many smaller ones
  fit, #2040); more leaves the projection not computed, with the reason.
- **Current digests.** `spec::cited_fact_digests` gives, for the cited ids,
  the canonical digest of every field of each node or edge of the snapshot
  the read took. It is the digest an investigation records for a fact it
  reads, so a finding is eligible exactly while its facts are as the run
  read them.
- **Reads (as built).** `curated.md` gains a section, "Findings accepted
  as curated", written only when a finding is accepted. `best-effort`
  prints each eligible finding as one numbered statement with its title,
  claim kind, `T3 / InferredWeak`, producing role, source support, identity
  and citation ids, then its limitations. Each cited node or edge is an
  assertion of the artifact, once, so every printed id has a row in the
  fact index. An eligible finding that cites a fact the export does not
  admit is named and not printed. `verified-only` prints none and states
  how many it left out. Every accepted finding that is not eligible is in
  a table with its state, reason and changed and missing ids, the same in
  both modes, and its statement and title are printed nowhere. The
  Workbench read and `curated-json` carry the same split as data:
  `findings` (shown), `findings_omitted`, `findings_listed` (identity,
  role, state, reason and ids only) and `finding_counts`. One rule,
  `spec::curated_finding_shown`, decides what every read shows.

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

## Reconciliation (AC-1020)

Reconciliation is a review flow, not a new state (ADR-0061 decision 9). The
review dialog of an accepted record offers two steps beside its state:

1. **Resolve gap again on current source.** This opens the record's Gap in
   the Resolution Strategy dialog, so a new task is assembled from the
   current capture and a new proposal is staged for its own review. Nothing
   in the old record changes, and re-accepting the old record is not
   offered: acceptance never re-certifies changed evidence (#385 AC 7).
2. **Withdraw acceptance.** This rejects the old record at its current
   review revision (compare-and-set, SPEC-03), so it leaves the projection
   at the next curated revision. Its saved basis and review history stay
   readable in proposal history.

A rejected record offers neither step. Until it is withdrawn, an old record
for the same Gap keeps its own state, and a new accepted record with a
different edge makes both `conflicted`.

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

- Findings (#1746 requirement 2): the projection core admits them, the
  host stores and reads their curation reviews, and the export, the
  Workbench read and the MCP reads list them (ADR-0074, AC-0843 to
  AC-0845). The Investigations surface offers the curation review beside
  the follow-up review, and the Workbench curated section draws the list
  (AC-0846).
- Cardinality is one accepted edge per Gap until slot cardinality can be
  proven from the graph.
- The Gap predicate is duplicated in `context-hub` and `spec`; a parity test
  in `spec` keeps them equal.
- A Workbench decision on a derived statement, or one that has gone stale,
  makes the curated read unavailable over MCP until the compile it needs is
  accounted for in the worker's limits. #1834 did not change this.
- The fact index is not readable over MCP, so the citation ids `curated.md`
  prints cannot be resolved there.
- Export costs (#1834, AC-0659): the export commands run off the UI thread;
  the assessments of one export share one graph copy at one store revision;
  the extra compile for a Workbench decision that matches no stored fact runs
  once per store revision, graph content hash and set of such decisions, and
  its answer is kept in memory only. Measured in a debug build only; not
  verified for responsiveness in a running app.
- `ContextStatus::AwaitingReconciliation` stays on the wire and in stored
  records unchanged; no surface shows it any more.
- The MCP `user-stories` artifact and the desktop export are built from
  different inputs today (the knowledge overlay). Requirement 1 of #1746
  holds for `curated.md`, not for that artifact.
