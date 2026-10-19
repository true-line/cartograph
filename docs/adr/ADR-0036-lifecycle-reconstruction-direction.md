# ADR-0036 — Lifecycle reconstruction as the product direction

- **Status:** Proposed in #572 (owner review)
- **Date:** 2026-10-19
- **Deciders:** Cartograph owner; implementation agents (Claude, Codex)

## Context

On 2026-10-19 the owner restated the product's end state, recorded as vision
elements V1–V9 in [VISION.md](../VISION.md): pull in an app, a set of repos or
one repo; gather everything deterministically first; build the knowledge graph
and embeddings on those facts; work back through the lifecycle (journeys and
use cases, documentation and architecture, test arrangements and validation);
visualize flows and the roles infrastructure plays; work with a team of agents
in the app that ask, raise issues and assign detailed work; let humans and
agents fill the holes with updates or plugins; stay easily extensible for new
languages; keep skills token-efficient and progressively disclosed.

Today's code is a recovery engine with a context hub in progress. Deterministic
T0 extraction is strongest for TypeScript; Python, Go, Java and Kotlin yield
symbols, imports, calls and endpoints; events and fetch sites exist only for
TypeScript. Terraform/AWS, Pulumi and OpenTelemetry feed the IaC and dynamic
tiers; the semantic tier is eval-gated and ephemeral; a bounded T3 broker and
durable specialists exist behind per-step consent. The spec compiler emits its
artifacts, but no producer outside `crates/spec` creates Capability,
BusinessFlow, DomainEntity or Actor claims, so user stories and journeys
compile empty while BusinessRule guarded-exit observations, ADR recovery and
technical flow tracing (every hop Confirmed, no business journey) do exist.
Cross-repo HTTP stitching is deferred; the plugin ABI is single-file
extraction; no MCP, ACP or socket server exists in product code; the
specialist loop re-serializes its whole ledger on every invocation; of the
SPEC-01 gates, H1 is implemented, H2 measured at 0/8 complete rules, H3 has
prerequisites only, H4 is in progress and H5–H7 are untouched.

The question is not whether to build toward the vision but how to order it so
the integrity spine survives: what is deterministic first, how derived meaning
is labeled, how curated views relate to facts, how work leaves the product,
and how extensibility is measured.

## Decision

1. **Deterministic-first acquisition.** For a scoped question or artifact,
   every applicable, available deterministic and observational acquisition
   runs first, and coverage, missing inputs, unsupported capabilities and
   stopping reasons are recorded before any interpretation. Missing
   producers are recorded as coverage limitations and separately
   prioritized; they do not prohibit a scoped, explicitly qualified
   interpretation or graph read. "Exhausted" always means the declared
   revision, question, scope, tools and inputs, never global completeness;
   runtime evidence may be unavailable or unapproved and then stays an
   explicit gap. Semantic retrieval never changes a fact's tier; a
   relationship asserted from similarity is a separate labeled claim; no
   model promotes a hypothesis into an established fact. This orders effort;
   it never blocks reads of what already exists.
2. **Lifecycle artifacts are projections with content acceptance.** Journeys,
   use cases, architecture and deployment views, and test arrangements are
   compiled views over the graph. Each has a content-acceptance definition:
   for a use case, an evidenced actor and goal, trigger, preconditions, cited
   paths, and decision and exception coverage checked against an
   independently specified expected set that records whether each
   expectation is implemented, documented intent, inferred or unknown. An
   artifact may be explicitly partial when a required field is unknown; an
   intended alternate path never becomes an implemented-path claim, and a
   lexical guarded exit is not by itself a business decision. Delivery gates
   test that content on named fixtures, never file existence or node counts.
3. **Derived meaning keeps its epistemic tier; the method is recorded
   separately.** The producing method (deterministic rule and version,
   model, human) is recorded distinctly from claim kind and from tier and
   confidence. A structural projection with a complete structural
   justification (a deployment unit and the handlers it routes to) may be a
   T0 fact under its named, versioned projection rule. A rule-derived
   semantic candidate (naming a capability, assigning an actor goal, calling
   a hop sequence a journey) is an interpretation at T2 with InferredWeak
   confidence and a visible rule-derived method badge, never written through
   the T0 writer; InferredStrong only under a declared independent
   validation contract. LLM-produced interpretations remain T3/InferredWeak,
   including after deterministic validation or formatting, and never rise
   through acceptance. No new confidence label is introduced. A DomainEntity
   is not a persistence DataEntity by default; an explicit MAPS_TO carries
   the evidence. Human acceptance changes review status only.
4. **One curated view, revision-bound.** The H3 curated projection is a view
   with its own identity over unchanged facts. Its read contract carries the
   view identity (recovered or curated), the curated revision, accepted
   proposal identities, the unchanged producing tier and confidence, review
   status, source support, and freshness or conflict state. The UI, exports
   and agents read the same contract; re-ingest marks stale and conflicted
   claims instead of dropping or upgrading them.
5. **Omissions are classified and coverage is a matrix.** Every absence is one
   of: unsupported extractor, missing input, unresolved relationship,
   contradictory or stale evidence, genuine product or requirement gap, each
   with its remedy. Coverage is reported per language × framework ×
   capability with conformance fixtures, never as a boolean "supported".
6. **The work packet is the handoff unit.** Assigning work means generating a
   versioned, evidence-backed packet (identity, intent, evidence, work,
   acceptance, handoff, return). Rendering it as a GitHub issue follows the
   fleet SOP shape and retrieves routing at publish time. Generation,
   publication and execution are separate authorized actions; a packet never
   grants authority; evidence freshness, review readiness and execution state
   are independent dimensions; a completed worker is not a verified
   improvement until re-ingest shows the change.
7. **Bounded investigation before orchestration.** The named specialists
   stay; expanding to a general chat, roster or pool happens only after a
   useful bounded investigation (compact discovery, selected source read,
   cited finding, auditor review) is accepted with a real provider and
   measured tokens and time. MCP ingress and ACP execution then attach to
   the same durable coordinator; passive read-only transport may be designed
   earlier without closing H4/H5; protocols never confer trust.
8. **Progressive disclosure is a measured product property.** Model-facing
   context is a compact view with exact evidence on demand; the full ledger
   stays host-side; budgets come from measured baselines (tokens, bytes,
   useful-evidence fraction, calls, latency). This repository's own agent
   skills follow the same rule: a short entry with deeper references on demand.

9. **Test-arrangement links are qualified.** A discovered test, a subject
   dependency (imports and calls), observed execution or coverage (receipts
   only), assertion support, and proposed verification are distinct links; a
   "validates requirement" link carries its own evidence and interpretation
   qualification, and coverage alone never claims behavioral correctness.

## Consequences

- [ROADMAP.md](../ROADMAP.md) sequences the work; its workstreams carry the
  observable gates and the first issues. SPEC-00's schema and artifact table
  gain a lifecycle layer through follow-up spec PRs; SPEC-01 stays the context
  hub contract; the field-level contracts for lifecycle artifacts, work
  packets and compact context arrive as their own specs.
- New user stories reserve acceptance criteria and test ids for the gates.
  Nothing here changes shipped behavior, so no Changeset.
- Open issues #239, #250, #255–#260, #385 and #429 are re-scoped by the
  roadmap rather than closed; a bounded T3 analyst's interpretation never
  inherits the tier of a deterministic tool it invoked.
- The below-ceiling confidence that `core-prov` already permits becomes
  documented policy in the follow-up spec rather than a permissive validation.
- Review tiers are unchanged (ADR-0029); this file stays owner-reviewed.
- Extends ADR-0019 (context hub, MCP/ACP) and keeps ADR-0007's integrity
  principles; supersedes nothing.
