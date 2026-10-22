# ADR-0046 — Compact receipts, positive absence records and reclaimable retention

- **Status:** Proposed. Design r2 agreed with Codex (20261022T033000Z). Decision 1 (receipt contract v3) implemented; decisions 2–6 pending.
- **Date:** 2026-10-22
- **Deciders:** Cartograph maintainers; Claude implementation, Codex cross-review
- **Issue:** #725 (follows #722/#726; serves AC-0288 in #727)

## Context

After #726, large TypeScript repositories recover, but most of their facts can't be cited. On morluto/rea, 41,969 rule facts and all 68,754 declarations have no receipt. Each range repeats the full captured-file identity twice: once in `evidence` (repo, path, offsets, commit) and once in `captured.file` (source, capture, path, digest, length). That repetition is about 80% of receipt bytes.

The host receipt store is append-only, with a single 100,000 / 64 MiB ceiling across all sources. `forget` removes captures but never receipts. Every changed capture therefore adds a full receipt set, and an active user eventually fills the store.

Read-only MCP must distinguish *forgotten*, *omitted* and *missing* evidence. Today only "unavailable" can be said honestly.

## Decision

1. **Receipt contract v3** (`ts-primary-v3:`, domain `cartograph:ts-primary-receipt:v3\0`).
   - The content is v2 plus one `commit_sha`.
   - Each range is *compact* `{role, index, byte_start, byte_end}`. (Implementation note: r2 also allowed an *explicit* `{role, index, file, evidence, captured}` form for cross-file ranges. The receipt validator admits only same-file, same-repo, same-commit ranges with equal evidence and capture offsets, so every valid range is derivable and an explicit range could never be canonical. v3 therefore has no explicit form; cross-file ranges, if ever needed, are a new contract version.)
   - A compact range's evidence and capture are derived: repo = `repo_key`, path = `file.path`, commit = `commit_sha`, captured = `file` with the same offsets.
   - **Canonical form is mandatory.** A range must be compact exactly when it is derivable. The decoder rejects an explicit range that would be derivable, a compact range whose derivation fails validation, duplicate `(role, index)` pairs, and unknown fields. So one receipt has exactly one valid encoding, and therefore one id.
   - Before materializing anything, the decoder preflights the expanded range count and bytes against the existing per-receipt bounds. Compact size alone never admits an expansion.
   - The decoder expands to the existing in-memory `ReceiptRange`, so resolver, inspection, MCP and history are unchanged.
   - v1/v2 decode, validate and keep their ids unchanged.
   - Tests: parity vectors (the same fact as v2 and as v3 expands to identical ranges, with different ids), plus malformed compact inputs (non-canonical, contradictory, duplicate, over-expansion).
2. **Positive omission records.**
   - Each record is `(basis id, FactKey digest, kind ∈ {declarations, rules, store_capacity})`, stored per source revision next to the coverage record, with a per-file count index. Lookup is `(basis id, FactKey) → Option<ReceiptOmission>`.
   - Writes go through checked count/byte admission. Owner-set numbers count against the same retention accounting as receipts, and they are reclaimed with their revision when it is superseded and unpinned.
   - **If a revision's omission records cannot all be persisted, the revision records `omissions_incomplete` instead.** The resolver then reports "missing" for unrecorded facts, never "omitted". A partial set is never presented as complete.
3. **Forget and pins.**
   - `forget` deletes capture bytes, as today. In the same transaction, under the retention guard, it reclaims exactly the receipt rows that the shared eligibility predicate (section 4) allows.
   - **Pinned receipts keep their metadata rows.** They are marked `capture_forgotten`, so a historical citation still resolves its identity and reports forgotten bytes instead of silently disappearing.
   - The tombstone records `(source id, source revision/basis, capture id, path, forgotten_at, receipts_reclaimed, receipts_retained_pinned)`.
4. **Absence resolution, scoped to exact identity.** In order:
   1. Try present evidence for the exact requested `(basis id, receipt id)` and its capture.
   2. Pinned receipt row with `capture_forgotten` → **forgotten**.
   3. Exact receipt id is a reclaimed row. A reclaim ledger keyed by receipt id records the tombstone it belongs to → **forgotten**.
   4. Omission record for `(basis id, FactKey)`, where that basis has complete records → **omitted**.
   5. Otherwise → **missing**.

   Classification never uses the current source or path. A reimport gets a new source revision, so an old tombstone can't touch it, and an unknown or forged receipt id reaches step 5.

   Tests: forget then reimport, an old pinned receipt, an unrelated missing id, and simultaneous omission and tombstone records for the same fact. The receipt-id step wins because it is more specific.
5. **Reclaimable retention (shared eligibility predicate).**
   - A receipt row is reclaimable iff (its source is forgotten, or a newer completed recovery of the same source supersedes its capture) and no saved investigation ref, work packet or retained citation pins its receipt id.
   - Forget and supersession both use this one predicate, inside the committing transaction, under the retention guard, with a bounded pin query.
   - The global ceiling stays as the upper bound. Per-source shares and all numbers are proposed with measurements and are the owner's decision.
   - The `receipt-store-capacity` reason points at forget/supersession only once reclamation demonstrably frees space.
6. **Extraction memory.** Receipts are admitted per file as they are produced, with no whole-repo intermediate vectors. The REA peak is a measured acceptance item.

MCP keeps `evidence_absence_reason_available=false` until steps 2–5 pass end-to-end.

## Consequences

- Expected receipt size is 5–6× smaller, so the REA citable fraction should rise from ~20% toward the majority under unchanged ceilings. To be measured before claiming.
- One more contract version to keep forever. Review condition: v3 must be decodable without the v2 producer.
- Retention becomes a mutable state machine. Pins are a correctness boundary: reclaiming a pinned receipt would break historical citation, so the shared eligibility predicate gets property tests. The reclaim ledger keyed by receipt id costs a small amount of space per reclaimed row, which is bounded and accounted for.
- No numeric policy changes without owner approval.

## Alternatives considered

- **Compress the receipt column.** It saves bytes, but each range would still repeat the identity, decode would still be heavy, and nothing about absence reasons improves.
- **Raise the global ceiling.** That defers exhaustion and doesn't fix forget.
- **Infer omission from a missing binding.** Rejected. That is the false certainty AC-0288 forbids.
