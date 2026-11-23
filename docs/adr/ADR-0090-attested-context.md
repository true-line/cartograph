# ADR-0090 — Attested context fills evidence gaps, labelled as attested

- **Status:** Accepted (coordinator, as the owner's delegate, 2026-11-23)
- **Date:** 2026-11-23
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #2463; #1748, #1749, #1750, #1752; ADR-0036; ADR-0058;
  ADR-0061 (decisions 1 and 10); ADR-0064 (decisions 1, 4, 9 and 10);
  ADR-0066; ADR-0074; [SPEC-12](../SPEC-12_curated-projection.md);
  [SPEC-15](../SPEC-15_authored-sections.md)

## Context

Some gaps cannot be closed by static recovery: runtime-computed values,
build-generated code, shadowable names, deployment facts, and anything that
needs interpretation. These gaps surface when a document is drafted or
maintained. An engineer, or an agent working with one, often knows the
answer. Today that knowledge has no way to reach a drafted section:

- An accepted proposal whose evidence is not captured source text is
  `unverified`. It is listed but not printed (SPEC-12).
- A `section-writer@1` claim may cite only admitted facts (ADR-0064
  decisions 1 and 4).
- SPEC-15 excludes free-form chat and any new tier or confidence label.

So the drafting phase can either leave a section undrafted or risk an
unsupported statement. The closest thing to evidence available is what
investigations and work between agents and engineers established. That can
be used, but only if it is recorded and labelled as attested and never
passed off as recovered fact.

## Decision

1. **A fourth record kind: attested context.** An attested record answers
   one target: a Gap (its slot, as in SPEC-12) or a section address
   (SPEC-15). It holds a statement (at most 2,048 UTF-8 bytes), its target,
   optional supporting citations (ADR-0058 fact ids, source spans,
   documents), the content hashes of the facts around its target when it
   was recorded, and its attestations. It has its own table. Its identity
   covers the attested content only (target, statement, supporting
   citations and recorded hashes), never the attestations, so a
   confirmation, dispute or withdrawal leaves its citation id unchanged and
   a draft that cites it always resolves to its current standing. It is not
   a proposal, a draft or a finding, and it reuses none of their tables or
   decision types.
2. **Every contributor is named, by contributor id.** A record's
   attestations are an append-only history, oldest first, and none is
   without a contributor. A contributor may attest more than once (an
   engineer confirms and later disputes or withdraws); each contributor's
   latest attestation sets the standing, and earlier ones are kept. The
   first attestation states the statement.
   - An **agent attestation** records the role and its definition
     fingerprint, the model and provider, the producing run or chat, and the
     contributor id of the engineer who directed it.
   - An **engineer attestation** records a verb (`stated`, `confirmed`,
     `disputed` or `withdrew`), the time, the conversation or investigation
     it came from, and the engineer's contributor id.
   - An attestation always references a contributor id, never "the
     engineer". The contributor table is the only place that resolves an id
     to a display name and an optional stable handle (for example a Git
     author identity).
3. **v1 has one engineer per project, and the model assumes none.** v1 lets
   a project set exactly one engineer profile, once. Adding several
   engineers later must need only new contributor rows, never a migration
   of attestations. That later version will add: choosing the acting
   engineer, the same identity across machines that share a project, and
   disputes between engineers shown with who holds which position. v1 does
   not build these.
4. **Identity is project app data, never public text.** Contributor names
   and handles are entered by the project's users and stored only in the
   project's app data. They are never written to the ingested target, the
   repository, the base graph, a commit, an issue or any fixture. The
   project's own outputs (Workbench, Gaps register, compiled documents,
   exports, MCP reads) name the contributors of each attested claim,
   because those outputs belong to the project. Tests and committed
   examples use placeholder contributors only.
5. **Attested is a label, not a tier and not a fact.** An attested record is
   never written to the base graph, the base graph hash does not change, and
   its Gap stays open and counted (R-INT-1, R-INT-3, R-INT-4). It carries no
   tier and no confidence. Every surface renders it visibly differently from
   Confirmed, Inferred and Agentic content, with the word *attested* and its
   contributors.
6. **Drafters may cite it by its own citation id.** An attested record has
   an `A-` citation id: `A-` followed by the first 16 lowercase hex
   characters of its identity, distinct from the `F-` fact ids of ADR-0058.
   A drafting run may be admitted the attested records whose target is in
   its scope, and a claim may cite them beside facts. This amends ADR-0064
   decisions 1 and 4: an admitted citation is a fact *or* an eligible
   attested record.
7. **Printing.** A claim that cites an attested record is an attested claim.
   - `verified-only` prints no attested claim and states per section how
     many it left out, as it does for drafted claims (amends ADR-0064
     decision 10).
   - `best-effort` prints it annotated as attested, with each contributor
     and verb (for example "stated by section-writer@1 (run …), confirmed by
     <engineer>"). An engineer is named by display name, an agent by role and
     run. An `agent-attested, unconfirmed` claim (decision 8) is printed with
     that label and its contributing agents, never as engineer-backed.
   - The fact index of ADR-0058 decision 6 gains the attested records a
     document cites, under their `A-` ids, so the export still fails on a
     citation without a row (amends ADR-0064 decision 9).
8. **Lifecycle states in the curated projection.** Accepted records are
   projected as a fourth kind (amends ADR-0061 decision 1). Each record gets
   the first state that applies:
   - `withdrawn`: its latest engineer attestation is `withdrew`;
   - `disputed`: an engineer disputes a statement an agent made, or a
     confirmation is followed by a dispute. It is shown as disputed and
     never printed as settled;
   - `already-covered`: a later ingest recovered the same relation for its
     Gap deterministically;
   - `unresolvable`: its target or a supporting citation resolves to
     nothing in the current graph;
   - `stale`: a recorded hash differs from the current graph, as in
     ADR-0064 decision 8;
   - `eligible`: none of the above.

   Agents may fill a gap with other agents, without an engineer. A record
   whose attestations are all from agents, with no engineer `stated` or
   `confirmed`, gets its own visible status, `agent-attested, unconfirmed`,
   in place of `eligible` (the earlier states still apply first). It is
   projected and admissible to drafters. `verified-only` omits it and counts
   it; `best-effort` prints it with that label and its contributing agents.
   It is never promoted to accepted, and never shown as engineer-backed,
   until an engineer states or confirms it; an engineer `disputed` makes it
   `disputed`. The curated revision and policy move to version 4. They gain the attested
   records with their state, so every reader sees one revision for all four
   kinds.
9. **Four capture paths, one review.**
   - From an investigation or agent chat: an agent proposes and the engineer
     confirms.
   - Between agents: one agent states and others attest, recorded as
     `agent-attested, unconfirmed` until an engineer confirms.
   - From the Gaps register: the engineer states an answer.
   - Over MCP with the task grant (ADR-0066): staged as a proposal for
     review in the app, like drafts.

   An MCP session cannot confirm, and confirming is never exposed over MCP.
10. **Acceptance still never upgrades.** ADR-0064 decision 11 holds for
    attested claims. Citation membership is not entailment (ADR-0064 decision
    12). The engineer's confirmation is a statement of what the engineer
    knows, not a proof, and the documents say so.

## Consequences

- SPEC-15 no longer excludes every new label: *attested* is the one
  exception, and it is defined here. Tiers and confidence labels are
  unchanged.
- The curated revision changes for every project the first time version 4
  is read, including projects with no attested record.
- A gap answered by attestation stays in the Gaps count and is shown as
  attested beside it. Coverage and the score-0 export are unchanged.
- Contributor identity becomes project data the app must guard like other
  private project data. It must not leak into repository-facing artifacts.

## Alternatives

1. **Accept unverified proposals into print.** Rejected: it erases the
   difference between recovered and asserted, which R-INT-5 and SPEC-12
   exist to keep.
2. **A new tier above Agentic.** Rejected: tiers describe how a fact was
   recovered from the system. Attested content is not recovered from the
   system, and a tier would let it be counted as coverage.
3. **Anonymous attestation, or the engineer as a profile field on each
   record.** Rejected: a reader must know who stands behind each claim,
   disputes between engineers need named positions, and a per-record field
   would need a migration when several engineers arrive. Contributor ids
   cost nothing now.

## Slices

Each slice is one PR with its acceptance criterion (odd ids from AC-1071),
defined under US-0047 in `docs/user_stories.md`.

1. **Contributor model and attested record** (AC-1071): types, identity, the
   `A-` id and the pure admission check, in `crates/agents`.
2. **Store and review** (AC-1073): the table, the attestation verbs as
   compare-and-set reviews, and the single engineer profile.
3. **Curated projection** (AC-1075): the fourth record kind, its states, and
   revision and policy version 4.
4. **Printing and reads** (AC-1077): D1 in both modes, the fact index rows
   and the MCP read.
5. **Capture paths** (AC-1079): Gaps register, investigation and chat, and
   MCP staging.
6. **Workbench and Gaps surfaces** (AC-1081), with stories.
7. **Pilot** (AC-1083): on SimpleLogin, a gap answered by an agent and
   confirmed by the engineer produces an attested claim in D1 `best-effort`
   that names the agent run and the engineer. The claim is absent from
   `verified-only`, and the graph hash and the gap count are unchanged.
