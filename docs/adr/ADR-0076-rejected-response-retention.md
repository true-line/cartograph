# ADR-0076 — A failed investigation keeps a bounded, redacted excerpt of the response it rejected

- **Status:** Accepted
- **Date:** 2026-11-21
- **Deciders:** Cartograph lead (delegate); Claude implements
- **Related:** #1616; US-0032 / AC-0725, AC-1403; ADR-0071

## Context

When a provider response fails action decoding, the task stops with a closed,
text-free diagnostic (AC-0725). It names the decode stage or semantic rule and
the error class. The response itself is dropped: no `investigation_responses`
row is written for a rejected response. A pilot failure therefore can't be
attributed from the profile alone. Someone has to reproduce it, and on a
local model that often isn't possible.

Accepted responses are already stored whole. A rejected response is held to a
lower bar only because it failed validation, and some of those validations
exist for privacy: a response rejected as `sensitive_output`, or for copying
supplied source (`source_replay`), must not be kept.

## Decision

1. **One record per failed task.** When the terminal cause is a rejected
   response (`ActionDecode` or `InvalidResponse`), the terminal transaction
   also writes `investigation-rejected-response` schema 1. It holds the step
   id, the size and content hash of the whole response, and a retention. The
   record goes in its own immutable table, `rejected_response_records`. That
   table is created on first use and isn't counted toward task or store
   occupancy, the same way the refusal diagnostics sidecar works (#1707).
2. **Redact first, then cut.** The whole response goes through
   `core_redact::redact_text` before anything is cut, so a secret that
   straddles the cut is still caught. Control characters other than newline
   and tab become spaces. The excerpt is the first 1,024 bytes on a UTF-8
   boundary, with flags for whether it was truncated and how many redactions
   were made.
3. **Withheld causes keep no text.** If the terminal diagnostic says the
   response was rejected as `sensitive_output` or `source_replay`, the record
   is `withheld` and keeps only the size and hash.
4. **Never decisive.** If a record can't be built or stored, nothing is
   written and the task still fails: the insert runs in a savepoint that is
   rolled back on error. The terminal status, message and diagnostic stay
   exactly what a plain stop records. Historical tasks have no record.
5. **No new surface.** Nothing in the app or in exports reads the table yet.
   It is for pilot inspection of a profile. A reader in the task detail is a
   later decision.

The admitted action's canonical body (the second gap in #1616) is a separate
slice under this ADR, with the same rules: it is bounded, kept in its own
table, and never decisive.

## Consequences

- AC-0725's rule that no response text is kept is narrowed for rejected
  responses: a bounded redacted excerpt is kept, except for sensitive or
  replayed output.
- The profile grows by at most about 8 KiB per failed task, outside the task
  allowance.
- Redaction uses pattern matching. It covers the same secrets as question
  history (`redacted_history_text`), and no more.
