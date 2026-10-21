# ADR-0040 — Immutable investigation budget and admission policies

- **Status:** Proposed
- **Date:** 2026-10-21
- **Deciders:** Cartograph maintainers (owner numeric/time decision approved; integration pending)

## Context

Vendure prep4 at merged `1ec3b1cc2ba77b14fdfd70c3a43550f69ede5655`
selects 36 source ranges plus coverage inventory per role. Production measurement
requires at least 37 tool actions and 38 model invocations. Analyst and auditor
requests are 49,757 and 49,668 bytes; each envelope is 48,737 bytes. The existing
profile permits eight actions/invocations and 8 KiB requests/envelopes, and
production normalization rejects both proposals.

All agreed evidence families have matches, including the local transition
configuration. Lexical coverage remains incomplete: 610 bytes of keywords,
punctuation, documentation and the explicitly named oversized object opener are
not selected evidence. Semantic sufficiency and executable admission remain
unproven. The preparation and measurement made no provider calls or tasks; see
[#598](https://github.com/true-line/cartograph/issues/598#issuecomment-6317650533).

Limits are enforced independently by request admission, task storage, replay,
ledgers, prompts, worker dispatch and consent. Raising a harness counter or a
single default would leave contradictory enforcement and could invalidate
retained tasks. Existing immutable specialist prompts also state the old
limits explicitly.

## Decision

Use a closed registry of immutable named policies. Callers select a known
identity; they cannot submit arbitrary numeric limits. Keep admission bounds
(request bytes, envelope bytes and raw obligation counts) distinct from
runtime counters and per-invocation transport limits. The exact selected
policy must be bound into task intent, consent and durable evidence.

Keep investigation-v1, existing request and envelope encodings, role definitions,
fingerprints and frozen acceptance controls unchanged. The default remains v1. The opt-in policy uses a separately versioned request
and envelope family when larger admission bounds would change the old contract.
Mint domain-analyst@8 and evidence-auditor@8 for that policy rather than editing
@7 in place. Preserve target identities if their individual semantics and bounds
remain unchanged. Do not version unrelated records solely for symmetry.

Resolve the selected policy consistently at raw admission, normalization,
staging, nonce reuse, task creation, retained-task validation, replay, prompt
construction and dispatch. Reject unknown identities and saved limit values
that differ from their immutable specification. Admit complete raw requests
before deduplication or partition cloning. Legacy projection helpers must not
reinterpret a new request as a v1 request.

Keep invocation and token reservations committed before dispatch. Validate
journal capacity and terminal headroom before accepting a profile's promised
work. Static accounting for prep4's straight successful path is 227 local
events: six per source read, five for inventory, three for setup and three for
finish. Cloud consent adds two per invocation, giving 303 on that path. These
are code-derived estimates, not observed execution or approved ceilings. Event
ceilings and reserved terminal slots must resolve from the immutable policy;
retain 96 and the current reservations for v1. Task/store byte capacities and
sealed per-invocation input limits remain independently authoritative and must
be checked with the opt-in path before claiming it executable.
Consent binds the actual per-call payload and limits; more aggregate calls
do not automatically enlarge the per-call transport contract. The UI must
advertise explicit eligible policies and freeze the chosen request during an
uncertain retry.
The bounded transport's source-span allowance is explicit and comes from the
saved policy's evidence-item limit. Its legacy value of 12 is omitted from
serialized limits, preserving old payload and consent hashes; the v2 value of
56 is included in the prepared payload's consent identity. The independent
128 KiB serialized input ceiling still applies before firewall preparation.

The owner approved the measured opt-in proposal and retention of v1 time caps on
2026-10-21: Claude conversation `9e891118-de11-45fd-9e91-92d266702eee`, proposal
`d3cdd3a6-e50e-4e22-8c9d-c966207b3387`, human reply
`da696a3f-5d7d-4552-bdcf-a24ae2f012e6` at 13:54:21 UTC ("approve").
The approved profile is `investigation-v2`: 56 tool actions, 64 model invocations,
64 KiB requests and envelopes, 56 raw obligations and evidence items, 64 evidence
requests, 64 KiB acquired evidence, 512 journal events and 131,072 aggregate
output-token reservations. Individual source reads remain 8,192 bytes, inventory
32,768 bytes, repeated validation 128 MiB, selected facts 64, per-call output
2,048 tokens and input 128 KiB. Task storage stays 2 MiB with 256 KiB terminal
reserve; global storage stays 256 MiB. Active/consent/wall time caps remain
600/900/3,600 seconds. These values are capacity ceilings, not completion promises.
The v2 registry entry is enabled together with admission, persistence, transport
and execution integration and their synthetic boundary tests. Admission, coverage,
staged finish/refusal and replay validators must resolve the selected policy,
including derived attempt capacities. The immutable specialists COMMON prompt
embeds eight calls/actions; the @8 definitions state the new policy without
changing legacy text. This decision does
not authorize a provider run or establish semantic acceptance under #598.

Use prompt schema 7 only for v2. Exact source descriptors appear once in the
completion contract's source obligations, rather than also appearing in an
inspection-requirements array. Keep the inspection hash, progress and private
admitted targets. Sealing reconstructs the canonical targets from the protected
contract and verifies their identity against those private targets and the
inspection hash; progress remains tied to the ordered target identities.
Reject a second descriptor array. V1 retains prompt schema 6 and its exact bytes.
This removes redundant input rather than enlarging the 128 KiB payload ceiling.
The initial synthetic 56-source fixture with 112 attempt IDs required 147,041
serialized payload bytes before this change, even with no source text; its
request was 57,621 bytes. Capacity witnesses must cover the revised encoding
and still reject combinations that exceed the independently enforced ceiling.

## Consequences

Existing tasks retain their original limits and remain readable. New policies
require deliberate compatibility and boundary tests, including exact serialized
legacy goldens, rehashed saved-policy tampering, replay reservations, nonce
conflicts, raw oversized inputs and refusal before an excess dispatch.

The completion-attempt table migrates its structural sequence ceiling from 32
to 112 in the transaction that initializes a new completion investigation.
Only the exact historical schema is eligible; opaque record bodies and hashes
are copied unchanged, and rollback restores the original table and protections.
Historical tables remain readable before migration. Per-task policy admission
remains authoritative (16 attempt records for v1), independent of this larger
structural ceiling. Unknown or tampered schemas are rejected without repair.

Each role acquires its own required source and inventory evidence. Larger limits
must not be substituted for missing receipts, source preloading, hidden retries,
budget resets or auditor reliance on undisclosed parent history.

## Alternatives (≤3)

- **Raise global defaults:** changes old histories and immutable prompt meanings;
  rejected in favor of explicit opt-in policy identities.
- **Relax only the acceptance harness:** does not exercise production admission,
  restart or consent and cannot prove a usable investigation.
- **Batch source reads:** may reduce calls but requires its own exact-target,
  per-item charging and disclosure contract; it does not remove request admission
  or missing-evidence constraints.
