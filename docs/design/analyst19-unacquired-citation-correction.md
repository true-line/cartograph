# Candidate unacquired-citation correction contract

Status: implementation design for #1647, based on #1644. Owner delegate Claude confirmed the owner decision “Add a correction turn”: analyst@19, @18 frozen, existing limits, release0.61.1 followed by one separately reviewed model attempt. This document itself does not execute a provider or change policy.

Observed failure: the v0.61.0 30B attempt made two metadata queries, then submitted a supported finish referencing two known source-target IDs without acquiring either source. The deterministic rejection protected the result. A diagnostic-only fix is tracked by1644; acceptance additionally requires useful supported output.

## Eligibility

Resolve every rejected citation against the authenticated current task ledger and saved finishing disclosure. A known source target is not an admitted citation. Only a nonempty set of known, disclosed, unacquired source targets is eligible for the proposed feedback. Unknown, foreign-task, ambiguous, stale, malformed and mixed unknown IDs retain closed refusal behavior. An acquired target ID used instead of its actual citation ID is a different class and must not be silently normalized.

## Durable transition

For a new version only, authenticate the response and account its measured usage once. Record a typed feedback reason and bounded target identifiers, never rejected finding prose. Preserve the original invocation charge, remaining invocation/time/source budgets, exact task ownership and cancellation precedence. Store a once-only correction record bound to the original step and ledger digest. Reopen and duplicate publication must not grant another correction.

Expose the existing source-read action and the specific eligible targets in the next admitted disclosure. The host must not perform a read on the model's behalf. The model must explicitly choose acquisition, and ordinary source authorization, range, memory and policy checks still apply. If no subsequent action can be admitted within existing budgets, end with a legible refusal. A second invalid finish does not restart correction.

## Required verification

Synthetic production-admission tests must cover known-unacquired, unknown/foreign, mixed, acquired-target-versus-citation, late cancellation, stale revision, duplicate response, durable reopen, budget exhaustion and second invalid finish. Assert no unsupported result, no hidden source dispatch and conserved invocation/usage accounting. Old18 behavior and stored receipts remain readable and unchanged. A separately approved real-model run must demonstrate acquisition and a useful source-supported finding; passing synthetic tests alone is insufficient.

## Shared correction allowance

The new version's correction state and action-offering contract must be reviewed with the existing one-bounded finish correction rules. A finish-only offer cannot itself solve missing source acquisition. Do not label an acquisition-enabled corrective turn as finish-only or reset earlier refusal history.

## Implementation boundaries

The classifier belongs in the pure agents layer and must consume authenticated saved input rather than raw provider claims. The host owns once-only journal persistence, transactionality and response usage. The worker exposes typed feedback through the new version disclosure and follows ordinary acquisition dispatch. Existing18 prompt/fingerprint branches remain untouched. Desktop activation follows the existing version pattern; MCP default must be reported explicitly and preserved.

The correction enables acquisition actions, so it cannot reuse a finish-only mode without changing its meaning. Represent that difference explicitly and test the historical finish-only paths unchanged. Scope only #1647 beyond the copied #1644 dependency; rebase on its verified signed head before publication. No duplicate compile while #1644 owns the shared target.

## Verified existing transition constraints

The existing `ProtectedCompletionMetadata::next_call_mode_with_policy` already
returns `Choice` when the remaining invocation and token reservations cover an
acquisition choice plus the finish reserve; otherwise it returns `FinishOnly`.
A refusal does not inherently force finish-only mode. Prefer sharing the existing
immutable refusal chain and its one-continuation allowance, rather than adding a
second counter that could grant another correction after reopen. The implementation shares this chain and verifies the allowance in durable host tests.

Do not simply catch `InvalidCitation` and return `FinishDecision::Refused`.
`InvestigationAction::admit_finish` currently checks citation membership before
its prose replay check, and `admit_narrative` runs after result admission. Such a
catch would skip those later checks. The new-version path must explicitly validate
all rejected prose and narrative before persisting feedback, without constructing
an accepted result or treating target identifiers as admitted citations. Existing
version admission order and behavior must remain unchanged.

Historical refusal validation must still rederive the same obligation assessment
and bind the response, finish, disclosure, ledger and prior refusal hashes. Tests
must prove an earlier ordinary refusal consumes the shared allowance as well as
an earlier unacquired-target correction. No new numeric allowance is introduced.

## Compatibility and acceptance binding

Only the desktop default moves to analyst@19 when advertised with its valid policy. The MCP default is unchanged. Explicit restored selections, including @18, remain selected. The footer recognizes @19 as using the saved configured time limits.

The receipt family `request7-specialist19-prompt9-disclosure6-result7` is new. Historical @18 receipts and prompt bytes remain unchanged. Before any 0.61.1 model attempt, the frozen #598 protocol needs a reviewed pre-run amendment recording the specialist change from @18 to @19; implementation or synthetic tests do not authorize that attempt. Claude owns that amendment.

For @19, correction eligibility validates withheld prose before granting feedback. Source replay or sensitive output therefore retains its own error class instead of being treated as a recoverable invalid citation. #1644's fixed invalid-citation diagnostic applies only when InvalidCitation is the returned terminal class; no rejected prose is persisted. Historical @18 admission ordering remains unchanged.
