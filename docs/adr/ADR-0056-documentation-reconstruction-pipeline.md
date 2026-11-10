# ADR-0056 — Documentation reconstruction pipeline, two modes and the rebuild boundary

- **Status:** Proposed in #1745 (owner review)
- **Date:** 2026-11-09
- **Deciders:** Cartograph owner (goal restated 2026-11-09); drafted by Claude
- **Related:** #1745; ADR-0019, ADR-0036; VISION V10–V13; ROADMAP §10

## Context

On 2026-11-09 the owner restated the product goal:

> Drag in an app, or as much of its code as can be gathered. Use the
> deterministic engine to trace the infrastructure, code, UI/UX and
> documentation to gather facts and findings. The team of agents takes this
> work and works backwards to write the ADRs, product specifications and all
> the other enterprise documentation the app should have had since day one,
> had teams been properly managed or tooling been in place to avoid stale
> documentation. As drift is caught, the documentation is updated to the
> latest code, so that engineers can reconstruct or rebuild the app in a new
> language or to updated requirements.

Two clarifications came with it. Rebuild is something Cartograph enables
engineers to do; the app does not do it. Question answering exists for
projects that are already well run and do not need forensic recovery: there,
engineers and agents use Cartograph as a context hub and as a guide to the
full picture.

VISION (restated 2026-10-19) and ADR-0036 already carry the deterministic
foundation, the lifecycle artifacts and the agent team. Three parts of the
restatement are recorded nowhere:

1. **Who writes the documents.** ADR-0036 decision 2 makes every lifecycle
   artifact a compiled projection, and decision 7 limits agent output to
   staged proposals, findings and work packets. No agent can draft a document
   section, and nothing an agent produces reaches an exported document.
2. **Documentation that follows the code.** Drift is detected and listed. No
   document is marked out of date or redrafted when the code moves.
3. **The two modes and the rebuild boundary.** Without them the
   investigation acceptance track reads as the gate for the whole product,
   and NG2 reads as a limit of the current slice.

## Decision

1. **The product is a pipeline of five stages.** (1) Gather an application's
   code, complete or partial. (2) Trace it deterministically across
   infrastructure, code, UI/UX and existing documentation. (3) A team of
   agents writes the document set from those facts. (4) The documents follow
   the code as drift is caught. (5) Engineers rebuild or port the application
   from the result. Each stage builds on the one before it and never weakens
   it.
2. **Agents author document sections.** A section draft is a staged proposal
   with a document and section target. Every claim in it cites evidence the
   host admitted to the run. It is stored as T3 / InferredWeak and never
   written to the base graph. It enters the curated projection and the
   exports only after human review, and acceptance changes its review status,
   never its tier (R-INT-1, R-INT-2, R-INT-3). This extends ADR-0036
   decision 2: the deterministic projection stays the frame of every
   document, and authored sections are laid over it without replacing a
   deterministic statement. It extends decision 7: a section draft is a kind
   of staged proposal.
3. **A versioned catalogue names the document set.** For each document it
   states the reader, the sections, the deterministic inputs, which sections
   an agent may author and how the document is accepted.
4. **Documentation follows the code.** Each section records the graph
   revision and the facts it rests on. When a re-ingest changes those facts,
   deterministic sections regenerate and accepted authored sections are
   marked stale with the reason. Stale marking is a deterministic comparison.
   A redraft is a new staged proposal. Existing target documentation is read
   as evidence of stated intent: it is cited, compared with the code, and
   never promoted to a fact about the code. ADR-0045 holds the detailed
   design.
5. **Rebuild is enabled, not performed.** Cartograph writes nothing to the
   ingested target (NG1) and generates no target code (NG2). NG2 is a
   standing boundary of the product, not a limit of the current slice. The
   handoff to an engineering team is the document set with its evidence, the
   traceability matrix, the gap register and work packets. Work packet
   execution stays unoffered.
6. **Two modes over one graph.** In *reconstruction* mode the whole pipeline
   runs on a system whose documentation is missing or stale. In *context
   hub* mode a well-documented project uses the deterministic graph, the
   read surfaces and bounded investigations to answer questions and guide
   engineers and agents. Both modes obey the same integrity rules. The
   real-provider investigation acceptance gates the context hub claim.
   Content acceptance of authored documents against an independent expected
   set gates the reconstruction claim.
7. **The curated projection moves onto the critical path.** Nothing an agent
   writes can appear in a document until an accepted item is visible in the
   Workbench, the export and agent reads under one curated revision
   (ROADMAP W5).

## Consequences

- New workstreams W13 to W17 (ROADMAP §10), each landing through its own
  issue, spec and acceptance criteria. This ADR authorizes no implementation.
- VISION gains elements V10 to V13; V1 to V9 keep their ids.
- `verified-only` exports omit authored sections and count them;
  `best-effort` exports include them, annotated with tier, author role and
  review status (R-INT-5).
- The investigation machinery is reused for drafting: action admission,
  citation membership and the refusal protocol. A draft is a new finish
  payload, not a new loop.
- The base graph hash is unaffected by drafting, review or curation.
- The v1.0 criteria prove the context hub mode and the deterministic
  foundation. Whether v1.0 also carries a reconstruction criterion is an
  owner decision recorded on #1610.

## Alternatives

1. **Keep every document a compiled projection (ADR-0036 unchanged).**
   Rejected: where no rule reaches, the document is thin or empty, and the
   goal names agents as the authors.
2. **Promote an accepted draft to a confirmed fact.** Rejected: it breaks
   R-INT-1 and R-INT-2, and a reader could no longer tell a reviewed model
   statement from a proven one.
3. **Have Cartograph perform the rebuild.** Rejected by the owner: the app
   enables engineers to rebuild. NG1 and NG2 stay.
