# SPEC-15 — Authored document sections

- **Status:** Proposed for [#1748](https://github.com/true-line/cartograph/issues/1748). Slices 1 to 7 are implemented. The pilot's expected set is committed; its scored run is not.
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

**As built (slice 6).** `section-writer@1` is a generation-four completion
role beside `spec-writer@1`: the same request, obligations, limits, consent,
cancellation, journal and history, with its own definition and prompt
fingerprint. No other role's definition changes.

- *Request.* The run's question is the address, in one spelling:
  `Draft section 1.3 "Capabilities" of document D1 (catalogue 1.0) for scope
  system.` The `section_draft_question` command writes it. A start for the
  role is refused before anything durable unless the question names a
  drafted (A or P+A) section of the table under its catalogue title, for
  `system` or, where the section is written per subject, for one citation
  id. The role is not startable over MCP.
- *Claims.* The role returns one finding per claim. The existing finish
  admission applies unchanged (shape, bounds, citation membership in the
  run's ledger, replay and privacy checks) and additionally refuses a
  `proposed_design` finding for this role; such a finish goes through the
  existing refusal journal.
- *Citation set.* The system scope is every fact of the graph, which holds
  the section's frame. A subject scope is the subject node (it must carry the
  section's subject label), every edge that touches it and the node at the
  other end of each. Each fact is admitted by citation id with its current
  content hash.
- *Staging.* When the run ends completed with findings, the host maps each
  finding to a claim (the statement; `implemented`, `documented_intent` or
  `interpretation`; the citation id of each fact its citations rest on, once
  each), applies the slice 2 admission against the citation set and stores
  the draft with the role, its definition fingerprint, the job and the graph
  content hash. The newest returned draft of the same address that has no
  successor is recorded as replaced. An insufficient-evidence finish, a run
  that did not complete, a scope that names no subject and a claim citing a
  fact outside the scope stage nothing. Neither does a draft with a
  statement that spells out a file path or a code symbol of a fact it cites
  (`statement_names_source`; SPEC-16 rule 3, AC-0683). `section_draft_outcome` reports
  `staged` with the draft id or `no_draft` with a closed reason code, and
  stages the draft of a completed run that has none stored; it calls no
  model and storing identical content twice returns the stored record.

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

As built (`context_hub::curated`): `ProjectionInput` takes the stored
drafts and, per cited id, the current content hash of the fact it resolves
to; an id that resolves to nothing is absent. The projection reports every
accepted draft with its state and reason, and the totals. `superseded` and
`conflicted` look only at accepted drafts of the same address, whatever
their own state. The policy version is `curated-policy-v2`. The revision of
an existing project changes once and its content does not. Each accepted
draft carries its claims, which are what a document prints.

As built (host, slice 5): the application keeps the draft store in
`section-drafts.sqlite` beside the staging store. The curated read takes
every reviewed draft in one bounded read (the bounds of the staged records)
before any graph lock, and a store that cannot be read leaves the projection
not computed, as for the staging store. The current hash of each cited fact
is read from the nodes and edges of the snapshot being compiled
(`spec::cited_fact_hashes`), in one pass and only when a draft is accepted.
An MCP read session takes the same read and is refused as stale when a
draft is reviewed while it is acquired.

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
revision and the export mode. The host supplies the graph content hash on
its own, so the header states it even when the curated projection could not
be computed; it then gives the reason code in place of the revision. A
compile that is supplied neither states that. The document closes with its count of cited statements by tier.

As built (slice 5): a section's drafts are the accepted drafts the curated
projection lists for the document, the catalogue version and the section
number, in scope and then identity order. An eligible draft is printed as a
numbered list, one claim per line, each ending with its kind, `T3 /
InferredWeak`, the role, `human-reviewed: accepted` and its citation ids.
Every cited fact is an assertion of the document, once, so the export's fact
index check covers it. An eligible draft that cites a fact the export does
not admit (an inference the reviewer rejected, for one) is not printed, and
the section says so; no claim is shown beside a citation that does not
resolve. A draft in another state is named with its state and reason code in
both modes. A scoped draft is printed under its section with its scope; the
section is not yet repeated per subject.

In version 1 of the table a frame is one of four things: every admitted
non-Gap node of a label, each with its tier, confidence and citation id
(§1.2 `Actor`, §1.3 `Capability`); a summary of the traced flows (§1.6); the
count of Gap findings (§1.8); or a reference to the artifacts of the same
export that hold the statements (§1.4, §1.5). A business rule is rendered
only by the rule inventory, under its disclosure policy, and is never
repeated in the document. `verified-only` prints no claim and states per
section how many were left out. Existing artifacts are byte-identical,
except the fact index: `fact-index.md` and `fact-index.json` name the
document among the artifacts citing each fact its frames print.

## Surfaces

- **Export.** The new artifact, both modes. Cited ids go through the fact
  index check.
- **Workbench.** The document view shows each section with its kind, its
  claims and their annotations; each citation opens its evidence. A section
  offers "Draft this section" and the review actions.
- **MCP.** The artifact is readable as the export's bytes for the same
  inputs and mode. Drafting and review are not exposed over MCP.

As built (slice 7): `section_document` returns the section table of a
document with each section's kind and its stored drafts, system scope first,
at most 2000 drafts. `review_section_draft` records accept, reject or return
with an optional note at the revision the view read; a stale revision is
refused and no content changes. The curated Workbench read gains one row per
accepted draft with its state and reason and no claim text, so the view
states an accepted draft's state only from that read and states none when
the read is absent or older than the review. A citation selects the fact
index in the artifact list; it does not yet scroll to the fact's row.
"Draft this section" asks the host for the drafting question of the address,
puts it in the investigation composer with the Section Writer role selected,
and leaves starting the run to the user, so consent and limits are the
composer's.

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

As committed (slice 8): `docs/acceptance/w13/` holds the expected set for the
alias lifecycle over the API at the pinned SimpleLogin commit, 26 statements
across 1.2, 1.3 and 1.5 with their source lines, the scoring protocol and
`score.py`, which tabulates a reviewer's `scores.json`. No run has been
scored, so AC-0670 is open and T-0670 stays reserved until a result is
committed there.

## Open items

- The export compiles the base graph. `Capability`, `Actor` and
  `BusinessFlow` facts that live only in the knowledge overlay (ADR-0051)
  are not in it, so §1.2 and §1.3 list only base-graph facts. A draft that
  cites an overlay fact is `unresolvable` and is not printed: its citation
  would have no row in the export's fact index. Printing it needs the export
  to compile the overlay facts, which is not built.
- The D1 header does not yet carry the system name or the source revisions
  the catalogue asks of every document; the bundle holds them as
  `graph_basis`. §1.4 and §1.5 are not yet arranged per capability or rule.

- A refusal at staging (a scope that names no subject, a claim citing a fact
  outside the scope) is reported by `section_draft_outcome` as a closed
  reason code. It is not written to the run's refusal journal, because the
  run has already completed; the run's result keeps the findings.
- The citation set is not disclosed to the role as a list. The role reads
  through the existing queries, and a claim that cites a fact outside the
  set is refused at staging.
- The recorded hashes are those of the graph when the run ends. A fact that
  changed between a read and the finish is recorded with its new hash.
- A statement is checked for citation membership only.
- The pure check of slice 2 bounds a statement, refuses control characters
  and recognized secrets, and checks membership. The run-bound replay and
  privacy checks are the existing finish admission of the run (slice 6).
- Version 1 of the section table holds D1; each further document is a table
  change and, where a frame is missing, a projection.
- A draft whose scope exceeds the role's limits is refused, not split.
- Per-claim staleness is not reported; a draft is stale as a whole.
