# SPEC-15 — Authored document sections

- **Status:** Proposed for [#1748](https://github.com/true-line/cartograph/issues/1748). Slices 1 to 3 are implemented; the rest is not.
- **Date:** 2026-11-17
- **Scope:** US-0047 / AC-0663..0670; ROADMAP W13; v1.0.0 criterion 6.
- **Decision:** [ADR-0064](adr/ADR-0064-authored-document-sections.md).
- **Extends:** [SPEC-10](SPEC-10_specialist-investigations.md) (bounded specialist run), [SPEC-12](SPEC-12_curated-projection.md) (curated projection), [SPEC-03](SPEC-03_proposal-staging.md) (review by compare-and-set). `docs/DOCUMENT-CATALOGUE.md` version 1.0 decides which sections exist.

## Outcome and boundary

A user asks for one section of one catalogue document to be drafted. An agent
role drafts it from admitted facts. A human accepts, rejects or returns it.
An accepted draft whose cited facts still hold is printed in that section of
the compiled document, in the Workbench, a `best-effort` export and an MCP
read, under the one curated revision. Base facts, their tiers and the base
graph hash do not change.

Not in this specification: re-drafting on drift and the per-document stale
list (#1749); target documents as evidence (#1750); the handoff bundle
(#1752); any write to the base graph or the target; free-form chat; a new
provider, tier or confidence label.

## Terms

- **Address:** `(catalogue_version, document, section, scope)`. `document`
  is `D1`..`D11`; `section` is the catalogue number (`1.3`); `scope` is
  `system` or one ADR-0058 citation id.
- **Claim:** `{ statement, kind, citations }`. `statement` is at most 2,048
  UTF-8 bytes. `kind` is `implemented`, `documented_intent` or
  `interpretation`. `citations` holds 1 to 16 citation ids.
- **Draft:** an address, 1 to 64 claims in order, and for each distinct cited
  id the content hash of that fact when the run read it.
- **Frame:** the projected content of a section: every statement the compiler
  already produces for it.

## Section addressing

`crates/spec` gains a table of catalogue sections: document, number, title,
kind (P, A, P+A, H), whether it is written per subject, and for a P or P+A
section the projection that supplies its frame. The table is data checked
against the catalogue version it names; a section the table does not hold
cannot be addressed. Version 1 of the table holds D1 only.

## The draft payload and its admission

A drafting run is a SPEC-10 task for the role `section-writer@1`. Its request
names one address. The host:

1. Compiles the frame for the address and gathers the facts of the declared
   scope within the role's existing limits.
2. Admits each of those facts by citation id, with its content hash. This is
   the run's citation set. Source reads follow the existing admission and
   consent rules.
3. Accepts a finish only if it is a draft for the requested address in which
   every claim cites only ids in the citation set, every bound holds, and
   every statement passes the existing replay and privacy checks. Any other
   finish is refused through the existing refusal journal with a closed
   reason code. An insufficient-evidence finish is valid and produces no
   draft.

The host does not check that a cited fact supports a statement.

## Storage and review

Drafts live in their own table in the review store. A draft's identity is a
digest over its address, claims, cited hashes, the role definition, the
producing task and the graph content hash it was drafted against. Storing an
identical draft twice is idempotent. Payloads are bounded to 256 KiB.

Review takes a draft id, the expected review revision and a decision:
`accepted`, `rejected` or `returned`. It is an atomic compare-and-set and
increments the revision. A `returned` draft admits exactly one successor for
the same address; the successor names it, and cannot be returned. Reviews
never change a draft's content or provenance. Drafts and reviews reopen
unchanged after a restart.

As built (`agents::draft_store`): the table is `staged_section_drafts`, in
the review store's file beside the staged proposals. The identity also
covers the author role's name and the returned draft a successor replaces,
so a successor is never the same record as a first draft. Storing identical
content returns the stored record with its current review. The store
re-applies the admission rule to what it is given and refuses a draft that
records a hash for a fact no claim cites. A review note is at most 4 KiB.
A trigger refuses any update of the content columns, and a unique index
holds the one-successor rule. Whether an address is in the section table is
checked by the host that requests the run (slice 6), not by the store.

## Curated projection

`context_hub::curated::project` takes accepted drafts as a second record
kind. Each gets the first state that applies:

| State | Condition |
|---|---|
| `superseded` | Another accepted draft for the same address names this one as the draft it replaces |
| `unresolvable` | A cited id resolves to no fact in the current graph |
| `stale` | A cited fact's current content hash differs from the hash the draft recorded |
| `conflicted` | Another accepted draft has the same address and neither supersedes the other |
| `eligible` | None of the above |

The curated revision moves to `curated-revision-v2` and adds, in record
identity order, `(draft_id, review_decision, state, reason)` for every
accepted or rejected draft. A pending or returned draft does not enter. The
DR-4 fields are carried per draft as they are per proposal; `source_support`
counts cited facts.

## Compiled documents

The compiler emits D1 as `product-specification.md` (artifact id
`d1-product-specification`), built from the section table:

- **P:** the frame.
- **P+A:** the frame, then the claims of the eligible draft for the address.
- **A:** the claims of the eligible draft.
- **H:** a labelled blank.

Each printed claim shows its citation ids, `T3 / InferredWeak`, the author
role and `human-reviewed: accepted`. A section with no eligible draft says it
is not drafted and, if a draft is listed, gives its state and reason. The
header carries the catalogue version, the graph content hash, the curated
revision and the export mode. The hash and the revision are those of the
curated projection the host supplies; a compile that is supplied none states
that. The document closes with its count of cited statements by tier.

In version 1 of the table a frame is one of four things: every admitted
non-Gap node of a label, each with its tier, confidence and citation id
(§1.2 `Actor`, §1.3 `Capability`); a summary of the traced flows (§1.6); the
count of Gap findings (§1.8); or a reference to the artifacts of the same
export that hold the statements (§1.4, §1.5). A business rule is rendered
only by the rule inventory, under its disclosure policy, and is never
repeated in the document. `verified-only` prints no claim and states per
section how many were left out. Existing artifacts are byte-identical.

## Surfaces

- **Export.** The new artifact, both modes. Cited ids go through the fact
  index check.
- **Workbench.** The document view shows each section with its kind, its
  claims and their annotations; each citation opens its evidence. A section
  offers "Draft this section" and the review actions.
- **MCP.** The artifact is readable as the export's bytes for the same
  inputs and mode, from slice 5; until then it is not in the MCP catalogue.
  Drafting and review are not exposed over MCP.

## Slices

Each slice is one PR with its acceptance criterion.

1. **Addressing and D1 without a model** (AC-0663). The section table and
   `product-specification.md` with frames, "not drafted" markers and H
   blanks.
2. **Draft payload and admission** (AC-0664). The claim and draft types and
   the pure accept-or-refuse check, in `crates/agents`.
3. **Draft store and review** (AC-0665).
4. **Curated projection** (AC-0666). Draft records, states, revision v2.
5. **Printing accepted drafts** (AC-0667). Export in both modes, the MCP
   read, the fact index check.
6. **The drafting run** (AC-0668). `section-writer@1` in the investigation
   loop and the app command.
7. **Workbench** (AC-0669). Document view, draft request and review, with
   stories.
8. **Pilot** (AC-0670). Content acceptance on one SimpleLogin capability
   set.

Slices 1 to 5 use no model and are testable from fixtures.

## Content acceptance

For the pilot, an expected set for the chosen D1 sections is written from the
SimpleLogin repository before any drafting run and committed under
`docs/acceptance/`. It is never part of an ingested source, so no run can
read it. A reviewer who did not produce the drafts scores each printed claim
as supported, unsupported or missing against its cited sources. The scores
are committed whatever they are.

## Open items

- The export compiles the base graph. `Capability`, `Actor` and
  `BusinessFlow` facts that live only in the knowledge overlay (ADR-0051)
  are not in it, so §1.2 and §1.3 list only base-graph facts, and a draft
  citing an overlay fact could not resolve. Slices 4 and 5 must resolve
  citations and compare hashes over the base and overlay facts together.
- The D1 header does not yet carry the system name or the source revisions
  the catalogue asks of every document; the bundle holds them as
  `graph_basis`. §1.4 and §1.5 are not yet arranged per capability or rule.

- A statement is checked for citation membership only.
- The pure check of slice 2 bounds a statement, refuses control characters
  and recognized secrets, and checks membership. The run-bound replay check
  joins it when the drafting run is wired (slice 6).
- Version 1 of the section table holds D1; each further document is a table
  change and, where a frame is missing, a projection.
- A draft whose scope exceeds the role's limits is refused, not split.
- Per-claim staleness is not reported; a draft is stale as a whole.
