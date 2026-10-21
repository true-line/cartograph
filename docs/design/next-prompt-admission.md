# Next-prompt admission (#708)

Status: implementation in progress. This document does not claim that the admission guard, a new runtime family, or full-budget acceptance has shipped.

## Required behavior

A new versioned investigation runtime must prove that an action's resulting input fits before accepting the action. Use a sound bound before source access and an exact production-serialized assertion before acquisition publication. Preserve mandatory evidence, obligations and approved investigation-v2 limits. Refuse an unproven candidate class; do not substitute an optimistic estimate. If an unchanged-basis exact check contradicts its sound bound, stop with a distinct invariant failure preserving the receipt.

The proof covers both original and firewall-redacted payloads, protected metadata sealability, prospective completion attempts, progress, retained parent history and maximal continuation feedback. Byte capacity, provider request framing, generated-token reservations and unknown provider context capacity remain distinct. Early capacity refusal is not #598's full-remaining-budget certificate.

## Packet 1: pure derivation

Completion attempt IDs and validated records must have one derivation shared by prospective preparation and committed publication. The pure function accepts saved intent/step/response authority, an exact prospective ledger and a validated prior attempt count. It cannot attest that a read happened. Commit must reload its authority and derive the same result inside its transaction.

The existing append path retains its saved-response read, action binding, historical attempt validation, ledger lookup and durable event metadata. Incidental reads matching no obligation continue to create no attempt and need no ledger lookup. Existing family wire representations and content identity remain unchanged.

Protected completion metadata likewise needs a shared pure derivation after the database wrapper resolves invocation cutoffs and authoritative refusal history. Prospective metadata must include every newly derived attempt ID. A successful acquisition ledger alone is insufficient: unavailable reads and completion refusals can enlarge the next prompt without changing that ledger.

Feedback headroom is derived from the actual normalized obligation envelope: exact IDs and kinds, every obligation and the maximum serialized reason. This synthetic upper envelope is sizing data, never an actual refusal observation or evidence. Validate the bound through the enclosing prompt and CompletionPayload serialization; the raw stored-feedback cap is not its transport contribution.

## Current component evidence

The working implementation contains shared pure attempt and metadata derivation, a synthetic feedback sizing envelope, and a finishing-call requirement derived from validated saved refusal count and registered per-call token reservations. Neither requirement reserves resources by itself.

`core_redact::direct_source_json_content_bound` gives a checked sixfold bound for one directly serialized source string before and after `redact-text@1`. Its provenance proof covers expanding replacements and cross-pass consumption of canonical Bearer text. It excludes string quotes already in a payload skeleton, other fields, nested JSON and provider encoding. Changes to redaction patterns/order/templates or serializer behavior require re-proving the bound. Tests exercise actual redaction and serialization, sharp control-character bounds and arithmetic overflow.

Local component/regression validation: 172 agents tests passed (one existing local-provider test ignored), 98 production storage/ownership harness tests passed, 10 core-redact tests passed, and strict Clippy passed for the changed packages. These are local results; they do not establish Windows runtime success, a whole-prompt certificate, or real-provider acceptance.

The bounded LLM firewall and pure measurement now share the production wire preparation path. It reports raw string bytes, original/redacted payload bytes, original-action tuple bytes and egress-fingerprint tuple bytes. This includes actual redaction of system/prompt and span metadata. Measurements do not authorize dispatch and do not certify unknown source content or provider tokenizer/context capacity.

## Remaining integration

A private prepared acquisition binds request/family/policy, execution revision, original response/action, base ledger, prospective metadata and retained source/archive identities. Keep source guards through atomic commit. Source validation must still be durably reserved before source bytes are read. Lost or uncertain publication stops rather than replaying the operation.

Reserve a bounded latest-only capacity-feedback slot in every callable state, simultaneously with maximum applicable completion feedback and counter widths. Capacity refusal retains the already charged model call, increments a separate bounded refusal counter, and does not debit tool/raw-response usage or create unavailable/acquired evidence. Bind its durable event to the decoded action hash.

The latest capacity-feedback slot clears after an accepted acquisition; its cumulative refusal counter does not reset. Visible feedback requires a positive counter and binds the most recent refused action. A positive counter with an empty slot is valid. A refusal followed by an accepted acquisition must therefore stop presenting stale refusal advice while preserving its audit count.

Capacity feedback carries schema version 1, a validated action hash and one closed reason: `prompt_capacity`, `unproven_contribution`, `finish_call_reserve`, or `finish_token_reserve`. It carries no arbitrary prose. Its maximum is derived from every enum variant's serialized representation and the counter width at the registered model-call ceiling; adding a variant requires an exhaustive bound reproof. Actual feedback and synthetic sizing maxima remain distinct types.

Storage-headroom shortfalls use the existing terminal storage-exhaustion path, including a query/inventory commit race after a successful precheck. Tool/evidence budget exhaustion retains explicit terminal limit handling. Neither is reported as prompt-capacity refusal. These distinctions preserve the actual cause in the audit record and do not grant another model call.

Derive remaining finishing opportunities from saved completion-refusal allowance. Reserve those calls and their generated tokens separately. When only finishing reserves remain, expose a versioned finish-only phase and certify that prompt variant too. A tool proposal in that phase terminates explicitly. A partial finish is accepted only when the model supplies one and it satisfies the obligations; the host never fabricates success.

The proposed @10/request7/prompt9 family must leave @9/request6/prompt8 unchanged after #709 lands. Candidate commit, refusal journals, complete closure checks, version routing and UI reopening remain required before #708 can be called complete. Provider execution and new capture/preparation require their existing separate authorization.

## Agreed persisted admission contract

The new family pairs request7, specialist @10 and prompt9 with disclosure6 and
result7. Older families retain their exact encodings and hashes. Disclosure6
requires a closed admission stamp: schema1, derived mode, full pre-invocation
usage, cumulative capacity-refusal count, explicit nullable latest feedback, and
the canonical full turn-progress hash. Older disclosure versions forbid this
stamp. Result7 binds it through the disclosure hash under its new hash domain.

Mode derives only from persisted pre-call usage, completion metadata and policy.
Reopening must not recompute the historical mode from already charged usage.
Saved-step validation requires the exact existing invocation and its cutoff;
a missing invocation cannot mean all current journal entries. Shape validation
cannot authenticate a plausible invented count or feedback hash: the durable
capacity-refusal journal and acquisition order must prove those values.
Ordering uses explicit immutable invocation ordinals derived from the saved
pre-call model-invocation count plus one. Refusals and accepted input publications
resolve to that same ordinal domain; cross-table rowids, timestamps and table
sequences cannot establish ordering. Historical lookups exclude later
publications. The journal count must equal its sequence, and the previous-entry
hash chain is validated independently.

A tool proposal during finish-only mode is a distinct terminal protocol failure.
It preserves the model-call charge, causes no tool debit or source read, and
increments no capacity-refusal count. Tests must cover both call and token
reserve boundaries and the original action envelope, including replay actions.

Wire capacity and protected-field sealing are separate requirements. The actual
candidate must seal successfully, including the disclosure's own size bound.
Synthetic reserve projections must never become sealable receipts. Future
controlled-value classes need a proof of protected-field invariance through
redaction; successful byte sizing alone is insufficient.

The pinned captured-source class now has a pre-read wire bound for the exact
immediate action ID, profile and limits. It includes the appended source span,
compact citation handle, ledger/registry hash widths, read progress and both
continuation modes. Repeated successful reads append their actual evidence
ordinal. Unpinned ranges remain unproven. The bound does not establish retained
source authority, protected-field sealing or atomic publication. Local validation
for this slice passed 194 agents tests (one existing provider test ignored) and
strict Clippy; transaction-scoped reservation accounting passed 19 focused native
tests and strict harness Clippy. These are component results, not full runtime or
provider acceptance.

## Internal transaction and source-access checkpoint

The internal task3/intent4 storage path now authenticates saved call and
capacity-refusal journals by immutable invocation ordinal. Owner-authenticated
refusal commits retain the model charge without changing the input ledger or
charging a tool. A fault injected after the final successful task write proves
that a failed transaction rolls back task, journal, response and events together;
a subsequent retry records the refusal once.

Captured-source access now consumes an opaque validation receipt issued only
after the owning transaction commits its validation-byte charge, witness, event
and task revision. The issuer reloads the pending reservation and checks its
saved response, action, descriptor, ledger and execution binding. The actual
adapter rechecks cancellation and execution ownership immediately before reading
captured text. A lost commit acknowledgement yields no usable receipt and no
read; a committed charge remains durable. This boundary consumes an existing
reservation. It does not yet implement production reservation admission,
settlement, storage claims or recovery.
Reservation-phase task loading currently revalidates the pending source slot,
including a full read-namespace preflight. This is an interim runtime cost; any
optimization must preserve the same transaction-scoped integrity checks.

The new acquisition-attempt writer takes the exact saved call and appends only
the suffix derived from its saved response, candidate ledger and completed-tool
event. It compares projected records with fresh derivation inside the caller's
transaction and rejects duplicate invocation publication. The exact metadata
preview certificate separately binds complete current/candidate prompt inputs,
actual response bytes, usage, profile, limits and immediate next action ID.
Neither certificate nor journal records attest that graph/source acquisition
occurred; the host must retain the actual prepared acquisition and its guards
through publication.

Checkpoint validation: 156 native ownership tests passed before the final
attempt and rollback regressions were added; the later focused runs passed
three acquisition-attempt tests, five refusal-transition tests and three
ownership tests. Twelve app prepared-source tests passed, including real adapter
cancellation and lost-acknowledgement cases. Strict app and native harness Clippy
passed with these changes. These counts describe separate runs, not a claimed
full-suite run at the combined head. Atomic metadata publication, source
reservation/settlement and worker integration remain incomplete, and public
@10 routing remains disabled. No provider execution was performed.


## Staged storage and first-context integration checkpoint

Source reservations now carry separate validation and settlement allocations.
Pending logical occupancy includes both until the validation witness commits,
then only settlement, then neither after a validated disposition. Each writer
measures its actual task and global logical growth against its own allocation;
this includes task-body growth and the existing logical row/index allowance.
These are logical accounting bounds, not physical SQLite disk bounds.

Validation sizing and publication share one projection of the actual task,
witness and event. Event claims likewise retain three slots before validation
and two afterward. The persistence guard preserves those slots for both ordinary
and cancellation events. Focused tests cover exact task capacity, a validation
allocation one byte too small with unchanged durable state after reopening,
and exact event-slot boundaries. Independent review found no blocking defects
in this claim kernel; it does not establish source-admission authority.

The initial-context writer publishes the archive, initial ledger and Ready state
inside the owner transaction, after checking the genuine prepared input and
retained-source callback. The invocation writer separately authenticates Choice
or FinishOnly mode, saved metadata and actual preparation; cloud Consent replay
is validated before an invocation exists without relaxing saved-call validation.
Tests exercise Preparing through the first invocation and rollback boundaries.

Source settlement now has shared pure projections and a production-serialized
maximum for acquired, unavailable, released, discarded, invariant-defect and
unknown outcomes. Actual settlement persists those same projections and checks
its allocated bytes and counts. Component tests cover successful source and
unavailable continuations and terminal accounting. This does not complete the
production source-reservation admission, host/controller integration or recovery
acceptance. Public @10 remains disabled and no provider was executed.

Combined local checkpoint: all 197 native ownership tests pass, including the
21 reservation and 10 validation tests, and strict all-target Clippy passes for
both the app and native harness. Formatting passes. These results do not prove
Windows runtime behavior, real-provider acceptance, or public runtime readiness.
