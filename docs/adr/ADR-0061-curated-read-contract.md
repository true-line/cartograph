# ADR-0061 — The curated read contract

- **Status:** Proposed; owner review
- **Date:** 2026-11-12
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1746; #1745; #385; US-0021 / AC-0108; ADR-0036 decisions 3 and 4; ADR-0051 decision 2; SPEC-03; SPEC-09; [SPEC-12](../SPEC-12_curated-projection.md)

## Context

ADR-0036 decision 4 asks for one curated view, bound to a revision, that the
UI, exports and agent reads share. Nothing implements it. A survey of the
code for #1746 found:

- Four review stores and no identity across them: staged proposals
  (`proposals.sqlite`), legacy `agent_decisions`, Workbench
  `assertion_decisions`, and investigation finding reviews.
- Accepting a staged proposal writes the decision and nothing else. A test
  asserts that the export is byte-identical after acceptance.
- Only rejected Workbench assertions reach the compiler. A decision whose
  inference changed on re-ingest stops matching without any record.
- MCP reads report `h3_curated_available: false` from four literals.
- "DR-4" is named in ROADMAP and SPEC-10 and defined nowhere.
- The knowledge overlay is disposable, replaced on every publish, and
  withholds its facts when the base moves. It cannot hold curated state.

Existing specs also constrain the design. SPEC-03 and #385 say a record whose
evidence is not verified cannot activate curated context, and that accepting
again never certifies changed evidence. SPEC-03 says a finding review is not
curation. SPEC-09 says the basis assessment must not be stored as a single
"fresh" flag.

## Decision

1. **The curated projection is computed, not stored.** It is a pure function
   of four inputs: the base graph, the staged proposals with their reviews,
   the Workbench assertion decisions, and the policy version. No new store
   holds curated facts. The review stores stay the durable state, so the
   projection survives a restart by being recomputed from them.
2. **The curated revision is a content identity.** It is a digest over the
   policy version, the base graph content hash, and the ordered set of
   reviewed records, each with its identity, decision and assigned state
   (decision 4). The same inputs give the same revision on any surface. A
   review, a re-ingest that changes the graph, or a policy change gives a new
   one. It is not a counter.
3. **DR-4 is defined here** as the field list of ADR-0036 decision 4. Every
   curated read carries: the view identity (`recovered` or `curated`), the
   curated revision, and per item its record identity, producing tier and
   confidence unchanged, review status, source support, and state.
4. **Every accepted record has exactly one state**, assigned by the host at
   projection time and never stored:
   - `eligible`: all the conditions of decision 5 hold.
   - `unverified`: its evidence is not all captured primary-source evidence,
     or it is a version 1 record.
   - `stale`: its basis no longer matches the current graph or its selected
     associations, or its captured bytes are unavailable.
   - `already-covered`: a base fact now fills the slot it proposed.
   - `conflicted`: another accepted record proposes a different target for
     the same slot.
   - `unresolvable`: the Gap or an endpoint it names is no longer in the
     graph.
   Only `eligible` records enter curated content. Every other accepted record
   is listed with its state and the reason. None is dropped, and none is
   substituted.
5. **Eligibility is conservative.** An accepted staged proposal is eligible
   only when: it is a version 2 record; every evidence item is captured
   primary-source evidence that is still readable; the graph and association
   comparisons of SPEC-09 are both unchanged; the Gap and both endpoints
   exist; no base fact fills the same directed slot; and it is the only
   accepted record for that Gap. One accepted record per Gap is the
   cardinality rule until a slot's cardinality can be proven.
6. **Curated items never change base facts.** An eligible record appears as a
   curated item beside the recovered facts. It keeps Agentic/InferredWeak. It
   is not written to the graph, does not resolve its Gap in the base graph,
   and does not enter the base graph hash. The recovered view stays
   separately addressable.
7. **Export modes.** Every curated item is InferredWeak, so a `verified-only`
   export contains none and states how many it left out. A `best-effort`
   export includes them, marked.
8. **Workbench assertion decisions join the same revision.** Rejections keep
   filtering as they do today. A decision whose content hash matches no
   current assertion is listed as `stale`.
9. **Withdrawal is rejection.** The staging store has no cleared state after
   a first review. Rejecting an accepted record removes it from curated
   content at the next revision. No new review state is added.
10. **Finding reviews are not curation.** SPEC-03 stands: a finding accepted
    for follow-up has no fact shape and does not enter the projection. Legacy
    `agent_decisions` stay historical and do not enter it either.
11. **`h3_curated_available` is true only on a read that computed the
    projection** and returned the DR-4 fields.

## Consequences

- With the evidence in place today, few accepted proposals are `eligible`.
  Most are `unverified`, and the projection says so per record. That is the
  honest state until #385 extends captured evidence; the contract does not
  change when it does.
- The test that asserts acceptance leaves the export unchanged is replaced:
  an eligible accepted record changes a `best-effort` export, and an
  ineligible one changes only the listed states.
- `SpecBundle` gains a curated basis beside `graph_basis`. The base graph
  hash and every existing artifact's bytes are unchanged for a system with no
  accepted records.
- Computing the projection reads retained bytes (SPEC-09 assessment). It is
  bounded per read; a record whose assessment fails operationally is listed
  as `stale` with that reason, and is not eligible.
- #1746 requirement 2 names findings. This ADR does not deliver that part;
  it needs a fact shape for a finding first.
- The MCP `user-stories` artifact reads the knowledge overlay and the desktop
  export does not. That difference predates this ADR and is not resolved
  here.

## Alternatives

1. **A durable curated store with a revision counter.** Simple to read, but
   it duplicates review state, can disagree with it after a crash, and must
   be migrated whenever the policy changes.
2. **Write accepted proposals into the knowledge overlay.** One reader
   already exists, but the overlay is disposable, replaced by each publish,
   and hides stale facts instead of marking them.
3. **Admit every accepted record and annotate its evidence state.** More
   content appears at once, but it contradicts SPEC-03 and #385: acceptance
   would activate evidence nobody verified.
