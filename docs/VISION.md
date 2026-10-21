# Cartograph — Vision

> Restated by the owner on 2026-10-19. The plan to reach it is
> [ROADMAP.md](ROADMAP.md); the direction decisions it rests on are proposed in
> [ADR-0036](adr/ADR-0036-lifecycle-reconstruction-direction.md). SPEC-00 and
> SPEC-01 remain the contracts for what is built; this file says where the
> product is going and what must stay true on the way. The earlier staged
> roadmap (ADR-0007) is kept at the end for the record.

## The end state

Pull in an application — a set of repositories, or a single one — and get back
what a team would have produced while building it: the journeys and use cases,
the documentation and architecture, the test arrangements and their
validation, the flows through the system and the roles the infrastructure
plays in them. Work on it with a team of agents inside the app: ask them
questions, have them raise issues, hand work to other agents with issues and
requirements detailed enough to execute. Then let humans and agents find the
holes together and fill them, with updates to Cartograph or plugins for it.

## Vision elements

These ids are the shared vocabulary of the plan, the discussions, and the
issues that implement them.

| Id | Element |
|----|---------|
| V1 | **Ingest unit.** An app, a set of repos, or one repo, dragged in; everything below is reconstructed from it. |
| V2 | **Deterministic-first.** For a declared question and revision, every applicable and available deterministic and observational acquisition runs first, and its coverage, missing inputs and stopping reasons are recorded; unavailable acquisition does not block a qualified read, and the semantic and agentic tiers fill what remains and say so. |
| V3 | **Embeddings and a knowledge graph** built on those deterministic facts, as retrieval indexes and labeled inference aids, never as a substitute for them. |
| V4 | **Work back through the lifecycle.** Journeys and use-case planning, documentation and architecture, test arrangements and validation: reconstruct the typical artifacts of an enterprise or consumer app. |
| V5 | **Visualize the flows** and the roles the infrastructure plays in them. |
| V6 | **A team of agents in the app.** Ask questions, raise issues, assign work to other agents with near-perfect, detailed issues and requirements. |
| V7 | **Humans and agents fill the holes.** Identify what is missing and close it with updates or plugins. |
| V8 | **Easily extensible** for new language support. |
| V9 | **Token-efficient, progressively disclosed skills**, for the agents in the product and the agents that build it. |

## What must stay true at every stage

- **Integrity spine first.** Inferred is never presented as confirmed
  (R-INT-1..5). A rule-derived candidate and a model inference are each
  labeled as what they are; human acceptance changes review status, never the
  producing tier. This is the trust that makes agentic work on top safe.
- **Local-first, explicit egress.** Nothing leaves the machine without
  per-tier consent; redaction precedes egress.
- **Gaps are product.** The honest remainder, classified by what it would
  take to close it (unsupported extractor, missing input, unresolved
  relationship, contradictory or stale evidence, genuine product gap), is what
  the next human or agent works on.
- **No writes to the ingested target** (NG1, ADR-0019). Work leaves the
  product as packets and issues; implementation happens in a separately
  authorized checkout and comes back through re-ingest.
- **Determinism and provenance.** Re-ingesting the same commit and source registration with the same producers and configuration yields identical graph content; every claim
  cites revision-bound evidence.

## How the thesis evolved

The original thesis ([ADR-0007](adr/ADR-0007-agentic-sdlc-dogfood.md)) saw two
missing pieces for an agentic SDLC: a guide for people running one, and an
on-ramp for existing systems nobody can specify. Cartograph attacked the
on-ramp first: recover the specification with provenance, never hallucinating.
[ADR-0019](adr/ADR-0019-domain-context-and-agent-interoperability.md) then
superseded the starter-kit sequencing for a persistent domain context hub
shared by humans, in-app specialists and external agents ([SPEC-01](SPEC-01_context-hub.md)).

The restated vision keeps the integrity principles and the on-ramp, and widens
the deliverable from a recovered spec to the reconstructed lifecycle, with the
agent team inside the product rather than only around it. The recovery engine
and the context hub are its evidence foundation, not its end state.

## Beyond v1: targets without source

v1 reconstructs from source because business rules, user journeys and the
roles infrastructure plays can only be confirmed with the code. v2 adds
targets without source: a website, mobile app, desktop app or similar
artifact, broken down through observation (structure, bundles, network
traffic, binaries, runtime behavior). Observation layers on top of v1 and
fills its missing pieces. It stays separately labeled evidence that can support
or contradict a claim but never confirms a rule alone. See ROADMAP §8.

## Earlier staged roadmap (ADR-0007, for the record)

- **Stage 1 (SPEC-00 v1, M0–M10, delivered):** spec recovery only, with this
  repository dogfooding the target SDLC: every change traces to a US/AC, ADRs
  gate decisions, CI enforces traceability, agents do the work.
- **Stage 2 (starter kit):** arrange the recovered artifacts as a ready-to-run
  agentic SDLC workspace for the target system. Superseded as a prerequisite
  by ADR-0019; its content survives as the lifecycle artifacts and work
  packets of V4 and V6.
- **Stage 3 (SDLC in the core):** guided workflows, continuous re-ingest, and
  a porting workflow that preserves business use cases as executable
  acceptance criteria while the implementation modernizes. This remains the
  horizon beyond [ROADMAP.md](ROADMAP.md).
