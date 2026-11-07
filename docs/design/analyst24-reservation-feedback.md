# Analyst 24 reservation and combined correction design

Issue: #1701. Status: implementation design; no runtime or GA acceptance claimed.

## Captured source reservation

New capture receipts bind exact original and redact-text@1 JSON-content lengths
to the captured object digest and exact byte range. The capture producer computes
them while it owns verified bytes. Receipt canonical identity includes these
measurements. Historical receipts remain unchanged and retain conservative 6x
reservation; unsupported measurement/redactor versions cannot select exact sizing.

Before reservation, the host validates receipt identity, the measurement binding
and graph source binding without reading source bytes. Capacity certification
uses the maximum original/redacted contribution wherever either representation
can occur. Raw-string accounting and payload/action/fingerprint limits remain
independent checks. Analyst versions 20 through 23 retain their historical paths.

The existing reservation, durable validation charge, revalidation and access fence
remain in order. After the charged read verifies the full object digest, recompute
both escaped lengths using the production serializer and redactor. Each must be
no greater than its reserved metadata value. An understatement is an attempted-read
failure before any span enters context or a provider payload. A content-consistent
receipt alone does not prove its measurement truthful. No historical capture is
rewritten and no pre-charge source scan is permitted.

## Combined correction feedback

Schema 6 collects replay locations, limitation evidence IDs, enumeration locations
and configuration locations on the first eligible refusal. Existing detectors
retain their meaning. Collection visits every applicable location, including both
claim classes in one sentence; output is deterministically ordered and deduplicated.
Only structural locations and evidence IDs survive; rejected authored prose does not.

Emission is bounded by the existing feedback byte ceiling and includes an explicit
omitted count if all entries cannot fit. Counting continues after emission fills.
The overflow marker itself must fit. The existing single correction allowance
and invocation-bound witness cover the complete emitted envelope and count.
Schemas 1 through 5 keep their canonical representation and reopen behavior;
unknown schemas fail closed.

## Admission proof still required

Measure both completion_metadata.feedback and turn_progress, including nested
JSON escaping. Do not reuse a single-correction maximum for a list. Do not raise
numeric limits. The r4 step-15 conservative baseline is 136666 bytes against
131072; substituting original-content exact size alone yields approximately
126388, but this is not the combined admission result or a redacted-size proof.
Preserve the r3 actual-payload golden of 72369 bytes.

A potential tighter bound separates fixed citation guidance (measured from the
selected context, including arbitrary escaped fact identities) from structural
correction entries (finite codes, integer positions and validated evidence IDs).
The existing all-backslash carrier overestimates structural JSON. A replacement
must prove every embedding layer and redaction contribution; assuming that every
citation string is escape-free is invalid. If the joint bound does not admit the
r4 checkpoint, report the measured failure rather than changing policy.

The implemented schema-6 sizing component now uses a conservative envelope of
`max(existing citation encoded bound, ceil(feedback cap / 2))` backslashes.
Serializing its string produces at least the entire feedback cap plus two bytes,
all quotes/backslashes. Each further string embedding doubles all these bytes;
an already-encoded JSON value grows by at most that factor plus the same framing.
The existing citation bound remains intact for arbitrary escaped identities.
Schema-6 entries admit only fixed codes, integers and validated IDs; nested unknown
fields fail closed and the production redactor leaves these structural fields
unchanged. A near-capacity regression covers both feedback placements through the
production payload serializer and four subsequent string embeddings. This is a
component proof; the separate frozen-run joint admission evidence is recorded below.
The default analyst family remains unchanged.

## Required regression coverage

- Capture metadata identity, range and redactor binding; old-receipt fallback.
- Original escaping and production redaction expansion; tampered-down measurement
  fails after charging and before context installation.
- No pre-charge text access; existing full-object digest verification retained.
- Multiple replay and claim locations, both claim classes per sentence,
  deterministic deduplication, evidence-ID limitations and honest overflow.
- Schema 6 witness and both-slot sizing; historical byte/reopen goldens and
  unknown-schema rejection.
- Production joint r3/r4 admission measurements before any new model execution.

User-story acceptance criteria and test-map bindings cover this implementation.
This design grants no model execution or release authority.

The shared job layer accepts a borrowed native `SourceSizeBinding` interface,
implemented by the host prepared source over its receipt and retained leases.
The interface carries no deserializable caller-supplied measurements. Admission,
pre-publication verification
and settlement resolve sizes from that receipt only for analyst24, comparing the
complete target and base ledger. Missing/unsupported metadata retains the legacy
proof. The reservation basis commits the measured proof; actual charged bytes
still undergo the independent size check before context insertion. No schema or
numeric policy change is needed to persist the binding.

## Frozen-run offline production replay

The opt-in `frozen_run_production_admission_replay` diagnostic reconstructs seven
query pages through `ContextSnapshot` and qualified queries, and one inventory
page through the retained archive. Every page must match the saved ledger hash
and byte count. Full captured-object hashes and lengths, exact source slices,
and source citation references are checked before constructing prompts.

The historical r4 step-15 payload reproduces 67,252 bytes; its original 6x
certificate fails the unchanged limit. A counterfactual migration to analyst24
recomputes request and attempt identities, preserves the frozen evidence/order,
and runs production `prepare_v9_with_policy`, acquisition choice derivation,
firewall preparation, and measured-source certification. Both A and D are active:
choice original/redacted payload bounds are 126,365 bytes (4,707 bytes below
131,072), finish-only bounds 119,704, and egress fingerprint bound 126,796.
The source original/redacted JSON lengths both equal 2,130 bytes.

The r3 replay reproduces the historical step-15 payload of 68,653 bytes and the
actual next-payload golden of 72,369 bytes. Its analyst24 measured choice bound
is 126,359 bytes, finish-only 119,698, and source JSON lengths both 469 bytes.
These are offline representation proofs, not saved migrated task authority,
provider acceptance, or GA acceptance. The diagnostic takes an explicit local
`CARTOGRAPH_FROZEN_REPLAY` fixture and is ignored in ordinary tests; retained run
source text is not committed to the repository. Run it with
`cargo test -p agents frozen_run_production_admission_replay -- --ignored --nocapture`.

## Production capture compatibility

Fresh production directory captures measure original and redacted source sizes
before per-file declaration and aggregate receipt-budget admission. This changes
new receipt IDs and serialized capture bytes for **all** analyst versions,
including analyst20–23. The historical byte goldens cover fixed retained inputs;
they do not claim that newly captured receipts have the old bytes. Historical
single-file extraction remains available for explicit legacy fixtures.

Analyst24 uses the conservative 6x proof for receipts without supported size
metadata; it does not reject an otherwise valid legacy receipt merely because
measurements are missing. A measured proof without held receipt authority is
still rejected. Production directory capture now supplies the analyst24 host
reserve/read/seal/publication regression, and a separate legacy-receipt regression
covers the conservative fallback. The default analyst family is unchanged.

The authorized read-only #598 source comparison used 17 TypeScript files from
`598-v063/source-2cc309de`: both modes admitted the same 718 receipt facts with
zero omitted rule or declaration receipts. Legacy/measured retained bytes were
482,220/664,290; serialized bytes were 1,083,636/1,456,609. The measured capture
contained 3,962 measured ranges. No admission limit changed.
