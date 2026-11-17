# ADR-0064 — Authored document sections

- **Status:** Proposed; owner review
- **Date:** 2026-11-17
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1748; #1745; #1746; #1749; US-0047 / AC-0663..0670; ADR-0036 decisions 2, 3, 4 and 7; ADR-0056; ADR-0058; ADR-0061; `docs/DOCUMENT-CATALOGUE.md` version 1.0; [SPEC-15](../SPEC-15_authored-sections.md)

## Context

The document catalogue (version 1.0) names eleven documents and marks each
section as projected (P), authored (A), framed (P+A) or human (H). Nothing
produces an A section or the prose of a P+A section, and no export is
arranged as a catalogue document: the export is twenty-six artifacts that the
catalogue maps onto document sections.

What exists to build on:

- The specialist investigation loop (SPEC-10): versioned roles, host-admitted
  actions, citation membership, a finish that is refused when it breaks its
  contract, bounded budgets and per-step consent.
- The curated projection (ADR-0061, SPEC-12): one computed view with a
  revision, read by the Workbench, the export and MCP. It admits one record
  kind, an accepted edge proposal for a Gap.
- Citation ids and the fact index (ADR-0058): a stable `F-` id for each fact
  an export asserts, and an export that fails when a cited id has no row.

What does not fit as it is: a staged proposal is one edge for one Gap, and an
investigation finding is deliberately kept out of curation (ADR-0061 decision
10). A drafted section is neither.

## Decision

1. **A draft is a list of claims, and nothing else.** A section draft holds
   an ordered list of claims. Each claim is one statement, its claim kind
   (implemented behaviour, documented intent, or interpretation) and one or
   more citation ids. A draft has no text outside its claims: headings and
   order come from the catalogue. A claim with no citation cannot be
   expressed.
2. **A section has an address.** It is the catalogue version, the document
   id, the section number and a scope key. The scope key is `system`, or the
   citation id of the subject a section is written per (one capability, one
   decision). A draft targets exactly one address.
3. **Drafting is a bounded specialist run with a new finish.** A new role,
   `section-writer@1`, runs in the existing investigation loop with its
   existing action admission, budgets, consent and cancellation. Its finish
   payload is a section draft. The host refuses a finish in which a claim
   cites an id that was not admitted to that run, and applies the existing
   replay and privacy checks to every statement. No other role changes.
4. **Admitted citations are facts the export can resolve.** The host admits
   to a drafting run the facts of the section's projected frame and of its
   declared scope, each by its ADR-0058 citation id. A draft records, for
   every id it cites, the content hash that fact had when the run read it.
5. **A draft is its own record kind.** It is stored in its own table with a
   complete-content identity over the address, the claims, the cited hashes,
   the role definition, the producing run and the graph content hash it was
   drafted against. It is not an edge proposal and does not reuse that table
   or its decision type. Review is a compare-and-set on a revision, as in
   SPEC-03.
6. **Three review decisions.** Accepted, rejected, or returned. Returned
   allows exactly one successor draft for the same address, which names the
   draft it replaces. A successor cannot itself be returned.
7. **Accepted drafts enter the curated projection as a second record kind.**
   The projection assigns each accepted draft exactly one state, at read
   time: `superseded`, `unresolvable`, `stale`, `conflicted` or `eligible`.
   The curated revision gains the draft records and moves to a new policy
   version, so every existing reader sees one revision for both kinds.
8. **Stale is decided from recorded hashes.** An accepted draft is stale when
   any fact it cites has a different content hash in the current graph than
   the one the draft recorded. This is the input #1749 needs; listing stale
   sections per document and re-drafting them belong to that issue.
9. **Catalogue documents are compiled, over the existing projections.** The
   compiler emits a catalogue document as a new artifact. A P section is the
   existing projection for that section. An A section, or the prose of a P+A
   section, is the claims of the eligible accepted draft for that address,
   each printed with its citation ids, T3 / InferredWeak, the author role and
   the review status. Where there is no eligible draft the section states
   that it is not drafted, with the state of any listed draft. An H section
   is a labelled blank. A projected statement is never replaced or removed by
   a draft. The twenty-six existing artifacts are unchanged.
10. **Export modes.** Every claim is InferredWeak. `verified-only` prints no
    claim and states, per section, how many it left out. `best-effort` prints
    them annotated (R-INT-5). The fact index check of ADR-0058 decision 6
    runs over the ids the printed claims cite.
11. **Acceptance never upgrades.** A draft and its claims stay T3 /
    InferredWeak before and after review. Nothing is written to the base
    graph, the base graph hash does not change, and nothing is written to the
    ingested target (R-INT-1, R-INT-3, NG1, NG2).
12. **Citation membership is not entailment.** The host can check that a
    claim cites admitted facts. It cannot check that the facts support the
    statement. That is established by human review and by content acceptance
    (ADR-0036 decision 2), and the documents say so.
13. **The first document is D1.** The first compiled catalogue document is
    D1, the product specification, and the content-acceptance pilot is one
    SimpleLogin capability set in it, as the catalogue states.

## Consequences

- One more review store joins the curated revision. ADR-0061 decision 10
  stands: finding reviews remain outside curation.
- The curated revision changes for every project the first time the new
  policy version is read, including projects with no draft.
- A draft can go stale without its text being wrong: a cited fact's evidence
  span can move. The document shows the state and the reason; it never drops
  the section silently.
- Claims-only drafts read as lists, not essays. That is accepted for v1 in
  exchange for a checkable rule that no sentence is uncited.
- D1 is produced beside `user_stories.md` and `rule-evidence.md` until every
  consumer has moved (catalogue default 6).

## Alternatives

1. **Reuse the staged proposal record.** Rejected: its identity, validation
   and slot rules are specific to one edge for one Gap, and widening them
   would change the meaning of records already stored.
2. **Free prose with inline citations.** Rejected for v1: the host cannot
   tell an uncited sentence from a cited one without parsing prose, so
   requirement 2 of #1748 would rest on the model's formatting.
3. **Promote investigation findings into sections.** Rejected: SPEC-03 and
   ADR-0061 decision 10 keep finding review separate from curation, and a
   finding answers a question, not a section address.
