# ADR-0055 — Refusal diagnostic sidecars

- **Status:** Accepted design; implementation under review, release acceptance pending.
- **Date:** 2026-11-07
- **Deciders:** Cartograph owner, conveyed by Claude as delegate
- **Related:** #1707, #1708; US-0032 / AC-0287

## Context

Adding diagnostic bytes to charged investigation records can change a later
exact-fit admission, even when those bytes fit at the time of the refusal.
Diagnostics must explain existing decisions without changing execution budgets,
model requests, refusal dispositions, or withheld findings.

## Decision

Store text-free diagnostics in a separate immutable SQL sidecar outside existing
workspace capacity accounting. The owner authorized one new constant,
`MAX_REFUSAL_DIAGNOSTIC_BYTES = 4096`, covering the complete serialized sidecar.
Retain a deterministic prefix and explicit omission counts; when details cannot
fit, retain an explicit unrecorded-capacity marker. Absence is historical
compatibility, not a substitute for a new diagnostic record.

Bind refusal and authored-finish identities through a domain-separated digest
in an existing independent witness slot. Preserve charged record lengths and
model-facing feedback. Insert sidecar and refusal or final withholding in the
same transaction. Reopen rejects deletion, substitution and locally rehashed
tampering. Final withholding describes the current proposal's original indexes.

Diagnostics apply only to newly produced records of witness-enabled analyst
roles 22–25, as confirmed by the owner's delegate on 2026-11-07. Earlier roles
retain historical receipts unchanged: no diagnostic field or charged-row growth.
A future role adopts the independent witness through its own version bump.

Collector rejection retains a hash-bound `collector_rejected` marker with
`omitted_entries: 0` meaning the count is unknown. Diagnostic collection cannot
veto the independently authenticated refusal or withholding outcome. Storage
and authority validation still fail closed. Regression coverage injects collector
failure and compares model payloads, dispositions, capacity and provider counts.

## Consequences

Diagnostics have a bounded storage exception, but confer no evidence authority.
They contain closed codes, ordinal positions and token classes, never source
identifiers or refused prose. Existing numeric policies remain unchanged.
Whole-run tests must compare retained details and no-detail-room outcomes for
disposition, withholding, model bytes, later capacity and provider counts.
Historical receipts must reopen without rewriting saved bytes. Local component
tests do not establish real-provider acceptance or enterprise GA readiness.

## Alternatives

- Embed diagnostics in charged records: can change later capacity decisions.
- Drop diagnostics when full: loses the explicit new-record omission evidence.
- Store an unauthenticated sidecar: cannot establish independent replay integrity.
