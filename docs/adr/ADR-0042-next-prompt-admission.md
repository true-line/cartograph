# ADR-0042 — Prove continuation capacity before tool admission

- **Status:** Proposed; component implementation only
- **Date:** 2026-10-21
- **Deciders:** Cartograph maintainers; Codex implementation, Claude cross-review

## Context

Bounded query history does not bound all future investigation inputs. Source
escaping, inventory strings, attempt identities, actual parent history and
completion feedback can enlarge the next serialized prompt. Accepting a tool
before checking those contributions can leave acquired evidence without a
representable continuation. Existing request and resource limits alone are not
a proof of their combined representation.

Source validation is charged before source bytes are read. A speculative read
cannot be disguised as an uncharged fit check, and capacity refusal cannot be
reported as unavailable evidence. Historical runtime identities must retain
their existing behavior.

## Decision

Implement an explicit @10/request7/prompt9 family with a conditional admission
invariant. Keep approved investigation-v2 numeric limits unchanged. Certify the
initial callable state, each candidate success/unavailable state, both bounded
feedback forms and finishing variants using production serialization and sound
class bounds. Reject unproven classes before action acceptance or source access.
Preserve @9 and earlier families after publication.

For source text, bound its direct serialized contribution before reading and
keep real validation reservations. Prepare the exact acquisition once under
retained guards. Compare its actual serialized continuation against the proven
bound before atomic publication. A contradiction on an unchanged basis is a
terminal invariant defect preserving receipts, not ordinary capacity refusal.

Candidate identity binds the saved request/family/policy, execution ownership,
revision, response and decoded action, base ledger, prospective completion
metadata and retained capture/archive identities. Commit rechecks these and
rederives completion records from saved authority. No speculative rows become
visible. Uncertain publication stops without reexecuting a tool.

Reserve one latest-only capacity-feedback slot at maximal width in every
callable state alongside applicable completion feedback. Refused actions retain
already incurred model charges and have a separate bounded refusal counter;
they do not consume executed-tool/raw-response usage or create citations,
acquisition revisions or unavailable/completed evidence attempts.

Derive finishing-call requirements from saved refusal allowance, and reserve
per-call generated tokens separately. Expose finish-only mode in versioned
progress and certify its distinct prompt. A tool proposal there terminates
explicitly without a capacity-refusal increment or tool debit, retaining the
model-call charge. The host never synthesizes a successful insufficient-evidence result.

Bytes, provider framing, generated-token reservations and unknown tokenizer or
provider-context capacity remain separate. Component bounds and early refusal
do not prove full-budget fit or successful #598 acceptance.

Owner amendment, 2026-10-27 (#716): verified conditional safety of each admitted
prompt and its finish/refusal path replaces the full-budget-fit prerequisite.
The versioned [revision 4 supplement](../acceptance/expanded-conditional-safety-v4.md)
records verification requirements and outstanding evidence. Refused or partial
completion remains insufficient for #598 acceptance; existing limits and separate
execution authorization remain unchanged.

Storage admission uses a transaction-scoped preflight and one coherent capacity
snapshot, invalidated after any reservation, validation-charge or disposition
write. A disposition releases a storage claim only after its saved binding and
accounting are validated; row presence alone is insufficient. This interim
approach retains work proportional to historical read records once per fresh
transaction, bounded by store/record caps. A pending-only index is deferred until
the new task family's authoritative pending state is integrated and tested.

The storage contract fails closed on corrupt or inconsistent rows. Body hashes,
bindings and immutable triggers do not detect an internally consistent forgery
by a local writer able to rewrite the schema and all associated records. No
in-process watermark or stronger local-writer security claim is introduced.

## Consequences

Preparation and commit share pure attempt and metadata derivation, reducing the
risk of checking one representation and persisting another. Evidence integrity
and truthful resource accounting take priority over accepting every proposed
read. Conservative refusal can reject a read that exact bytes would have fit.

The current implementation includes a coherent saved completion snapshot,
internal prompt9 construction and feedback-reserve measurement, and borrowed
query/inventory preparation views retaining source guards. Source reservations
have immutable storage records and central capacity accounting. These components
do not yet establish the complete admission transaction: complete class closure,
new runtime routing, atomic candidate transitions and refusal journals remain
required. The design document records that scope; this ADR is not a
shipped-feature or provider-acceptance claim.

## Alternatives

1. Check only on the next loop: safely rejects overflow, but after acquisition.
2. Stage source reads before sizing: can protect publication but cannot satisfy
   the chosen pre-action guarantee without truthful pre-read charges.
3. Increase limits or omit required evidence: changes owner-approved policy or
   weakens the requested evidence contract rather than proving fit.
