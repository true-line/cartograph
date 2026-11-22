# SPEC-17 — Living documentation

- **Status:** Implemented for [#1749](https://github.com/true-line/cartograph/issues/1749): slices 1 to 5 are on `main`.
- **Date:** 2026-11-17
- **Scope:** US-0048 / AC-0671..0675; ROADMAP W15; v1.0.0 criterion 7.
- **Decision:** [ADR-0045](adr/ADR-0045-living-documentation.md).
- **Extends:** [SPEC-15](SPEC-15_authored-sections.md) (drafts, review, compiled documents), [SPEC-12](SPEC-12_curated-projection.md) (curated projection).

## Outcome and boundary

After a re-ingest, every accepted section that rests on a fact that changed
or disappeared is marked stale, with those facts named, in the Workbench,
the export and MCP reads. Its text stays in place, marked, until a reviewed
successor replaces it. A user can request the re-draft and see what changed.

No model decides what is stale. Nothing is written to the ingested target
and no target document is edited (NG1). No source is watched: a re-ingest is
started by a user.

## Dependency record

A section's dependency record is that of the accepted draft at its address
(SPEC-15): `cited_hashes`, the content hash of each cited fact when the
draft was written, and `origin.graph_content_hash`. P sections and frames
are compiled from the current graph and have no record.

## Stale computation

`context_hub::curated::project` already assigns each accepted draft a state.
It additionally reports, per accepted draft:

- `changed`: cited ids whose current content hash differs from the recorded
  one, sorted;
- `missing`: cited ids that resolve to no fact in the current graph, sorted.

`unresolvable` drafts have a non-empty `missing`; `stale` drafts have an
empty `missing` and a non-empty `changed`. A draft that is `superseded` or
`conflicted` reports the lists too, so a reader sees them whichever state
won. The curated revision is unchanged: it identifies the reviews, and the
graph basis identifies the facts.

As built (slice 1): `CuratedDraft` carries `changed` and `missing`, filled
in one pass over the draft's recorded hashes, which are ordered by id. The
curated Workbench read returns both lists on each draft row. `curated.md`
gains the section "Accepted section drafts", one row per accepted draft
with its document, section, scope, state, reason code and the two lists,
in both export modes and with no claim text. The section is written only
when a draft is accepted, so a project without one keeps its bytes. The
ids are printed as text and are not citations of the artifact: a missing id
has no row in the fact index by definition.

As built (slice 2): `product-specification.md` prints a `stale` draft in
its section in a `best-effort` export, under the stale line and the changed
ids, and each claim that cites one ends with `cites a changed fact`. The
claims keep their annotations and their citations resolve as before. A
`verified-only` export prints no claim, counts the stale draft's claims
among those left out and names the draft with its state. Both modes carry
`Stale sections:` in the header. One changed fact marks exactly the
accepted drafts that cite it, and a second compilation of the same inputs
is byte-identical (T-0672).

As built (slice 3): the Workbench draft card of an accepted draft shows
the curated read's `changed` ids as evidence buttons and its `missing` ids
as text under the state, and marks each claim that cites a changed id. A
missing id has no fact, so it opens nothing. A `stale` or `unresolvable`
draft offers "Re-draft this section", which opens the composer for the
same address; naming the accepted draft as replaced is slice 4. Nothing is
derived in the view: the two lists are the host's.

## Surfaces

- **Compiled document, best-effort.** A stale draft is printed in its
  section under the line `Stale: cited facts changed since this text was
  accepted`, followed by the changed ids. Each claim that cites a changed
  id ends with `cites a changed fact`. An unresolvable draft is not
  printed; the section states that and how many cited ids are missing.
  The ids themselves are in `curated.md`: every id the document prints
  resolves in the fact index, and a missing id does not.
- **Compiled document, verified-only.** No drafted claim is printed. The
  count left out and the stale list are stated.
- **Header.** After the as-of values: `Stale sections:` and the addresses
  (section number, scope and state) of every stale or unresolvable accepted
  draft, or `none`. Without a curated projection the line says the list is
  not known.
- **`curated.md`.** Each draft row lists its changed and missing ids.
- **Workbench.** The draft card shows the changed and missing ids under the
  state, and offers "Re-draft this section".
- **MCP.** The artifact read returns the export's bytes, so it carries the
  same marks. Re-drafting and review stay off MCP.

## Re-draft

A re-draft may be requested for an accepted draft whose state is `stale` or
`unresolvable`. The host checks the state against the curated read, then
starts an ordinary drafting run for the same address. When the run stages a
draft, the stored draft names the accepted draft as the one it replaces.

- The replaced draft stays accepted and in place until the successor is
  accepted. It is then `superseded` and stays in the store.
- Rejecting the successor leaves the replaced draft as it was and allows
  another re-draft.
- A draft has at most one successor that is not rejected.
- A successor is accepted or rejected and is never returned, as ADR-0064
  decision 6 says of every successor. To ask for another attempt, reject
  it and re-draft.

As built (slice 4): the `section_redraft_question` command takes the id of
an accepted draft and returns the ordinary drafting question for its
address, or a closed reason: `unknown_draft`, `draft_not_accepted`,
`draft_not_stale` (no cited fact changed or is gone) or `successor_pending`
(a successor that is not rejected is stored). The check compares the
draft's recorded hashes with the current graph, the same comparison the
curated projection makes. No new kind of run exists: when any drafting run
for an address stages its draft, the host names as replaced the newest
returned draft without a successor (ADR-0064) and otherwise the one
accepted draft of that address whose cited facts drifted and that has no
standing successor. Two such accepted drafts are in conflict and neither
is named. The draft store accepts an accepted draft as the replaced one,
holds "one successor that is not rejected" in a partial unique index, and
refuses to bring a rejected successor back while another stands. A table
created before this slice is rebuilt on open with its rows and sequence
kept. The Workbench shows a re-draft beside the accepted draft it
replaces.

## Section change

For a draft that replaces another, the host returns:

- claims added, removed and kept, where a claim is kept when its statement,
  kind and citation list are equal;
- cited facts added, removed, and those present in both whose recorded
  hashes differ.

The Workbench shows this beside the successor before it is reviewed.

As built (slice 5): `agents::section_change` compares two drafts and reads
nothing else. Claims are matched once per appearance; added and kept claims
are in the successor's order and removed ones in the replaced draft's. The
fact lists are sorted. The host adds `changes` to the document view: one
entry, keyed by draft id, for each listed draft that replaces a stored
draft. No separate command exists. The Workbench shows the entry on the
successor's card as a closed summary (claims added, removed and kept, and
how many cited facts differ) that opens to the claims and the fact ids. A
fact the successor cites opens as evidence; one it no longer cites is text.
The comparison applies to every successor, so a draft that replaces a
returned draft shows it too.

## Slices

Each slice is one PR with its acceptance criterion.

1. **Changed and missing facts** (AC-0671). The two lists in the curated
   projection, the curated Workbench read and `curated.md`.
2. **Stale in place** (AC-0672). The compiled document in both modes, the
   header list, and the fixture test: one changed handler marks exactly the
   drafts that cite it, twice the same.
3. **Workbench** (AC-0673). Changed and missing ids on the draft card.
4. **Re-draft** (AC-0674). The request, the successor of an accepted draft
   and its lifecycle.
5. **Section change** (AC-0675). The comparison and its view.

Slices 1 and 2 use no model.

## Open items

- A fact added to the graph that contradicts a claim does not mark it
  stale (ADR-0045, not decided).
- There is no human-edited section text yet.
- The stale list is per compiled document; D1 is the only one today.
