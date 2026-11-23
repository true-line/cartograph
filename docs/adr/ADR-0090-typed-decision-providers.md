# ADR-0090 — Typed-decision providers (TypeSafe Jev): built behind consent, deferred until a live eval

- **Status:** Accepted (coordinator as the owner's delegate, 2026-11-23). Decision on Jev: **defer.**
- **Date:** 2026-11-23
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #429, #258; ADR-0002 (escalation ladder), ADR-0019 (egress
  gate), ADR-0083 (per-read refusals); SPEC-00 §2, §13; R-INT-1, R-INT-2,
  R-INT-5

## Context

#429 asks whether TypeSafe Jev, a "System One" model, can fill the gap
between T0 and T3 at repository scale. Jev returns no prose. It takes a
state blob and typed questions (`Choice` up to 255 options, `Score`) and
answers each with probabilities and a confidence the vendor says is
calibrated.

The spike needs a measured answer on Cartograph's own labels: precision@1,
calibration, latency and cost, beside the local T2 resolver on the same
cases.

Jev is cloud-only and in limited early access (since 2026-09-15). The project
has no access and no API key, so no live call can be made. The vendor's
calibration claim is unverified.

## Decision

1. **A narrow typed-decision trait, not the completion trait.**
   `llm::typed_decision::TypedDecisionProvider` takes a `DecisionRequest`
   (state plus choice questions) and returns typed answers.
   - Every call goes through `EgressFirewall::decide`. The firewall redacts
     the payload with the same redaction as completions, builds an
     `EgressPreview`, and for a cloud provider needs the tier allowed by
     policy and a `ConsentGrant` for that exact preview.
   - A denied tier, a missing grant, a mismatched grant or an egress recorder
     refusal fails closed before the provider is called.
   - The payload's system line is `typed-decision@1`, so a completion grant
     never matches a decision preview.
   - A reply must answer each question once, choose one of its options, give
     one finite probability per option summing to 1, and give a confidence in
     [0, 1]. Anything else is an invalid response.
2. **Tier.** A typed decision is recorded at `InferredWeak` (the T3 ceiling)
   with its returned confidence (`semantic::decision_eval::DECISION_CONFIDENCE`).
   - It never produces, overwrites or upgrades a T0/T1 fact.
   - SPEC-00 defines T2 as local embeddings, and a cloud provider with
     unverified calibration does not earn `InferredStrong`.
   - Revisit only with a live eval that shows calibration on our labels.
3. **An eval harness against recorded answers.**
   `semantic::decision_eval` runs labelled candidate-selection cases through
   the firewall: one choice question per case, over the candidates plus an
   explicit `none` option, so "no acceptable target" stays expressible.
   - It reports precision@1, a 10-bin calibration table with expected
     calibration error, p50/p95 latency, reported input tokens and cost.
   - It reports these beside the local embedding resolver's top-1 on the same
     cases.
   - The checked-in fixture `crates/semantic/fixtures/decision-recorded-v1`
     replays recorded answers. It tests the mechanism, not Jev.
4. **Jev itself is deferred.** No Jev adapter ships, nothing is wired into a
   consumer (#237, #240, #244, H2), and nothing runs by default.

## What a live run needs

- Early access to Jev and an API key. The key goes in the OS secure store the
  existing cloud providers use, never in the repository or an agent-facing
  file.
- A `TypedDecisionProvider` for Jev's HTTP API: its endpoint, request and
  response mapping onto `DecisionRequest` and `DecisionResponse`, and its
  model identity recorded per call.
- The frozen #258 labels (`labels.jsonl`, sha256 `5fda19c5…5b02`, 180 Call
  cases) mapped into `DecisionCase`s from the locked evaluation store.
  - The validation split stays unseen until the run.
  - The local resolver is scored on the same cases.
- A consent grant per case through the existing consent flow, and the T2
  tier allowed for cloud only for the run, then reverted.
- Price per million input tokens from the vendor's then-current pricing,
  recorded in the report.

#429 stays open until a live run records its report in `docs/evals/`, and a
follow-up to this ADR adopts or rejects Jev.

## Consequences

- The firewall, consent and tier rules apply to typed decisions exactly as
  they do to completions, and a test pins each refusal path.
- A provider other than Jev with the same shape (a local classifier, for
  example) can be measured with the same harness.
- No production behavior changes.
