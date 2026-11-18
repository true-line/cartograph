# ADR-0074 — Findings accepted as curated

- **Status:** Proposed; owner review
- **Date:** 2026-11-18
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1746 requirement 2; #1745; #730; #385; US-0021 / AC-0843..0846; ADR-0036 decisions 3 and 4; ADR-0058; ADR-0061 (amends decision 10); ADR-0064 decisions 7, 8, 11 and 12; SPEC-03; SPEC-10; [SPEC-12](../SPEC-12_curated-projection.md)

## Context

#1746 requirement 2 says that accepting a staged proposal or a finding makes
it visible in the curated projection. ADR-0061 delivered proposals and left
findings out (decision 10), because a finding had no fact shape. ADR-0064
then added section drafts as a second record kind: a list of cited claims,
stale when a cited fact changes.

What a saved finding is today:

- One statement with a claim kind, a title, limitations and one to eight
  citation ids. The host admitted it only if every citation was admitted to
  the run. It is stamped Agentic / InferredWeak.
- Each citation of the result names one graph fact (a node or an edge) and
  carries the digest that fact had when the run read it. It may also name a
  source span and say whether that span is captured primary-source evidence.
- Its only review is the follow-up review of SPEC-03 (#730): accepted or
  rejected *for follow-up*. That review selects work. It validates nothing
  and activates nothing, and SPEC-03 says so in terms.

The owner decided on 2026-11-18 that findings reach the curated projection
through a curated acceptance of their own, not through section drafts.

## Decision

1. **Curation is a second review, separate from follow-up.** The follow-up
   review, its table, its decisions and its meaning do not change. A finding
   accepted for follow-up is still not curated. A new review, *accept as
   curated* or *reject*, is the only thing that brings a finding into the
   projection. Neither review requires or implies the other.
2. **The fact shape of a finding is one cited statement.** A curated finding
   is the saved finding's statement, claim kind, title and limitations, the
   role that produced it, and the graph facts its citations name. Each cited
   fact is named by its ADR-0058 citation id and carries the fact digest the
   producing run recorded. Nothing is written by the reviewer and nothing is
   supplied by a caller: the host resolves all of it from the immutable
   saved result.
3. **A curation review has its own store.** It is a table of its own in the
   review store, beside the section drafts. The first review of a finding
   records the finding there as the host resolved it from the immutable
   saved result: its content, its cited facts and their recorded digests.
   The record is keyed by the SPEC-03 finding review subject, which binds
   the result, its producing configuration and the result-local finding
   id, and its content never changes. Decisions are `accepted` and
   `rejected`. A change is a compare-and-set on a review revision. Rows are
   never deleted. It does not reuse the follow-up table or its decision
   type, and the investigation store is unchanged.
4. **Accepted findings enter the curated projection as a third record
   kind.** The projection assigns each accepted finding exactly one state at
   read time, the first that applies:
   - `unresolvable`: it cites no fact, or a cited fact is not in the current
     graph.
   - `stale`: a cited fact's current digest differs from the digest the
     producing run recorded.
   - `eligible`: neither.
   Only eligible findings are curated content. Every other accepted finding
   is listed with its state, its reason and the ids that changed or are
   missing. None is dropped and none is substituted.
5. **Stale is decided from what the run recorded, never from review time.**
   The digests compared are the ones in the saved result. Accepting a
   finding records no hash. So accepting a finding whose facts have already
   changed gives a stale finding, and accepting it again never certifies
   changed evidence (#385).
6. **One revision for all three kinds.** The curated revision gains the
   reviewed findings, each with its identity, decision, state and reason,
   and moves to `curated-revision-v3` under policy `curated-policy-v3`. A
   rejected finding enters with its decision only. A finding with no
   curation review does not enter.
7. **Acceptance never upgrades.** A curated finding stays Agentic /
   InferredWeak. Nothing is written to the base graph, the base graph hash
   does not change, and the Gap a finding is about is not resolved by it
   (R-INT-1, R-INT-2, R-INT-3).
8. **Source support is reported, not gated.** A curated read carries, per
   finding, how many of its citations rest on captured primary-source
   evidence and how many do not. A finding is not held back for citing a
   fact without a captured span. This follows ADR-0064 decision 12: the host
   checks that a statement cites facts that still stand, not that the facts
   entail the statement. That is what the human review is for, and every
   read says the statement is InferredWeak.
9. **A finding has no address.** Two accepted findings never conflict, and
   none supersedes another, even when they say the same thing. Each is
   listed under its own identity. A finding is not printed in a catalogue
   document: those sections take drafts (ADR-0064).
10. **Withdrawal is rejection.** Rejecting an accepted finding removes it
    from curated content at the next revision. There is no cleared state.
11. **Surfaces.** The Workbench curated section, `curated.md` and the MCP
    `curated` and `curated-json` reads list findings from the same
    projection under the same revision. `verified-only` shows no finding
    and states how many it left out. `best-effort` shows eligible findings,
    marked InferredWeak, with their citation ids (R-INT-5). The fact index
    check of ADR-0058 decision 6 runs over the ids the printed findings
    cite.
12. **Reads are bounded.** The host reads a bounded number of reviewed
    findings. A read that cannot finish within its bound leaves the
    projection not computed, with the reason, as for the other kinds.

ADR-0061 decision 10 is amended: a *follow-up* review is still not
curation; a curation review under this ADR is.

## Consequences

- The curated revision changes for every project the first time policy
  version 3 is read, including projects with no finding.
- A finding goes stale when any field of a cited fact changes, including a
  moved evidence span. Its text may still be right. The read shows the
  state and the changed ids; re-running the investigation gives a new
  finding with a new identity.
- A finding that cites a fact held only in the knowledge overlay is
  `unresolvable`, as a draft is (SPEC-15 open items).
- The same statement can appear twice, from two results. The reads list
  both. Merging them would need an address, which a finding does not have.
- One more review store joins the revision. A session that gathers reviews
  reads it under the same rules as the others.
- A curated finding is read from its own record, not from the
  investigation, so it does not depend on the investigation still being
  stored.

## Alternatives

1. **Findings reach documents only through section drafts.** No new review,
   but requirement 2 of #1746 would stay unmet for findings, and a finding
   would be visible only when a later drafting run happened to cite the
   same facts. Offered to the owner and not chosen.
2. **Let the follow-up acceptance curate.** One review instead of two, but
   it changes the meaning of every follow-up decision already stored, and
   SPEC-03 promised those decisions activate nothing.
3. **Gate on captured evidence, as for proposals.** Closer to ADR-0061
   decision 5, but most findings cite facts with working-tree spans or no
   span, so nearly every accepted finding would be listed and none shown
   until #385 lands. Drafts are not gated this way either.
4. **Record fact hashes at acceptance.** Simpler to compare, but acceptance
   would then bless whatever the facts are at review time, which #385
   forbids.
