# ADR-0045 — Living documentation: stale sections and re-drafts

- **Status:** Proposed; owner review
- **Date:** 2026-11-17
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1749; #1745; #1748; US-0048 / AC-0671..0675; ADR-0056 decision 4; ADR-0064 decisions 6, 7 and 8; ADR-0061; ADR-0058; ADR-0036 decisions 2, 3 and 7; [SPEC-17](../SPEC-17_living-documentation.md)

## Context

ADR-0056 decision 4 says documentation follows the code: a section records
what it rests on, a re-ingest that changes those facts marks an accepted
authored section stale with the reason, the comparison is deterministic, and
a redraft is a new staged proposal. It leaves the design to this record.

What #1748 built (ADR-0064, SPEC-15):

- A stored draft records the content hash of every fact it cites and the
  graph content hash it was drafted against.
- The curated projection gives each accepted draft one state at read time.
  `stale` means a cited fact's current hash differs from the recorded one;
  `unresolvable` means a cited id resolves to no fact.
- D1 names a draft in either state with a reason code and does not print it.
- A returned draft may have one successor that names it as replaced.

What is missing for #1749: the reason does not say which facts changed; a
stale section loses its text from the document; an accepted draft cannot be
replaced; nothing shows what changed between two drafts of a section; and no
test shows that the stale set is exact and repeatable.

## Decision

1. **The dependency record is the draft's own.** A section's dependencies
   are the citation ids and recorded hashes of the accepted draft at its
   address, plus the graph content hash of its origin. No second record is
   kept. A P section and the frame of a P+A section are projections of the
   current graph: they regenerate on every compile and are never stale.
2. **Staleness is a set comparison.** For an accepted draft, the projection
   compares each cited id's recorded hash with the current one and reports
   two sorted lists: `changed` (the id resolves, the hash differs) and
   `missing` (the id resolves to nothing). The state rules of ADR-0064 are
   unchanged. No model takes part, and the same graph, drafts and reviews
   always give the same lists.
3. **The reason names the facts.** Wherever a stale or unresolvable draft
   is shown, the changed and missing citation ids are shown with it. The
   reason code alone is not enough to act on.
4. **Stale text is shown, marked, and never substituted.** In a
   `best-effort` document a stale draft stays in its section, under a stale
   mark that names the changed ids, and each claim that cites a changed fact
   is marked. Its citations still resolve, so the fact index check holds.
   This replaces "named and not printed" for the `stale` state only. An
   `unresolvable` draft is still not printed, because a claim may not be
   shown beside a citation that does not resolve; the section names the
   missing ids. `verified-only` prints no drafted claim, as before.
5. **A document lists its stale sections.** The header of a compiled
   document carries the as-of values and the list of addresses whose draft
   is stale or unresolvable, in both modes. The Workbench and an MCP read of
   the artifact show the same.
6. **A re-draft is a successor.** A user can request a re-draft of an
   accepted draft that is stale or unresolvable. The drafting run is the
   ordinary one (ADR-0064 decision 3) and its staged draft names the draft
   it replaces. The replaced draft stays accepted and in place, marked,
   until the successor is accepted; it is then `superseded` and remains in
   the store as history. Rejecting the successor changes nothing else. A
   draft has at most one successor at a time, and a rejected successor
   frees the place. ADR-0064 decision 6 stands for returned drafts.
7. **A section's change is a comparison of two drafts.** Between a draft
   and the one it replaces the host reports claims added, removed and kept
   (a claim is kept when its statement, kind and citations are equal), and
   cited facts added, removed and changed. It is computed from the two
   stored drafts and nothing else.
8. **Nothing is written to the target.** Target documentation is never
   edited in place. A re-draft changes only the draft store (NG1).

## Consequences

- A `best-effort` document can print text that rests on a fact that has
  changed. It says so on the section and on each affected claim. A reader
  who wants no such text uses `verified-only`.
- The curated revision does not change shape. The changed and missing lists
  are derived from the graph basis and the recorded hashes, both of which
  the read already identifies.
- A draft goes stale as a whole. Claims that cite no changed fact stay
  printed under the section's stale mark and are not marked themselves.
- The draft store accepts a successor for an accepted draft. Whether that
  draft is stale is decided by the host that requests the run, from the
  curated read, and not by the store.

## Not decided here

- **Contradiction by a new fact.** A fact added to the graph that
  contradicts a claim does not make its draft stale: the draft cites no
  fact that changed. Detecting it would be a judgment and not a set
  comparison. The drift register keeps listing recovered contradictions.
- **Human-edited sections.** No surface lets a person edit a section's
  text. When one exists, its text carries a dependency record of the same
  shape and these rules apply to it.
- **Watching sources.** A re-ingest is started by a user. Scheduling and
  notification outside the app are out of scope (#1749).
- **Documents other than D1.** The rules apply to every compiled catalogue
  document; D1 is the only one compiled today.

## Alternatives rejected

1. Keep stale text out of the document. Rejected: #1749 requires the
   accepted text to stay in place until its successor is accepted, and a
   section that silently empties reads as never drafted.
2. Regenerate a stale section automatically. Rejected: every agent-written
   statement is reviewed before a document prints it (ADR-0064 decision 5).
3. Mark staleness per claim only. Rejected: a section's prose is reviewed as
   a whole, and a partial section would be printed as if it had been.
4. Let a model judge whether a change matters. Rejected: stale marking must
   be deterministic (ADR-0056 decision 4).
