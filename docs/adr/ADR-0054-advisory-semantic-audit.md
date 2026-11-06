# ADR-0054 — Advisory semantic audit

- **Status:** Proposed; implementation direction authorized by the owner on 2026-11-05 through Claude. ADR acceptance remains pending.
- **Date:** 2026-11-05
- **Deciders:** Cartograph owner; Claude and Codex implementation/review
- **Related:** #1654, #1651, #598, #1611; AC-0566

## Context

The development model audit withheld a candidate whenever any subject received
a non-pass verdict. Frozen replay evidence has not demonstrated sufficient
semantic discrimination: Qwen passed 104/120 known-good observations and flagged
1/21 known-bad observations (no bad subject met the two-of-three requirement).
GLM without reasoning passed 118/120 known-good observations and flagged none of
the 21 known-bad observations. The reasoning trial exhausted its 16,384-token
output allowance with invalid JSON under a test-only 900-second timeout; it did
not establish semantic acceptance. These results do not satisfy #598.

## Decision

1. New `candidate-evidence-auditor@3` contracts retain the existing evidence
   scope and single-call resource limits. Their outcomes are advisory. Every
   deterministically admitted, timely, uncancelled candidate is saved with
   either typed subject reviews or an explicit `audit not completed` reason.
   Bounded transport, request-timeout, length and invalid-output failures do
   not authorize a retry, repair, replacement provider or additional invocation.
2. Deterministic citation and limitation admission remain blocking. Privacy,
   ownership, cancellation and the overall task deadline remain blocking.
   A request timeout may precede that deadline; settlement rechecks elapsed
   task time before publication. Unknown owner-loss recovery never publishes.
3. State, invocation and response version 2 bind the advisory contract together.
   Version 1 retains its original blocking interpretation. Exact pinned terminal
   analyst definitions remain readable with unchanged role/trust metadata.
   Candidate bytes, result identity and original finish-response binding are
   unchanged. Saved detail exposes advisory feedback authenticated against the
   immutable receipt; findings and saved-chat views label it advisory, not proof.
4. Release acceptance still requires the independent review and evidence of the
   frozen #598 rubric. Neither this change nor a model pass establishes that
   acceptance. Rubric bytes and numeric policy limits remain unchanged.

## Consequences

A model non-pass can no longer suppress an otherwise admissible development
answer. Reviewers must inspect the advisory feedback and cited source evidence;
findings remain T3 / InferredWeak. Failure receipts contain fixed reason codes,
not raw model output. Accounted calls and reservations remain durable even when
reported token totals are unknown. Production dispatch stays disabled pending
its separate existing release gates. Re-promoting model verdicts to a blocking
requirement needs a new owner decision and demonstrated frozen-replay acceptance.

## Alternatives

- Disable the auditor: loses recorded feedback and execution evidence.
- Continue raising limits or changing models: lacks discriminating evidence and
  requires separate owner authorization.
- Audit each subject separately: changes invocation/resource policy and is not
  authorized by this decision.
