# ADR-0046 — Compact receipts, positive absence records and reclaimable retention

- **Status:** Proposed. Design r2 agreed with Codex (20261022T033000Z). Decisions 1 (receipt contract v3), 2 (per-fact omission records and lookup; see the implementation note under decision 2) and 3 (forget reclaim and pins) implemented; decision 4 is partially implemented for complete-mode MCP exact-binding forgotten and current-fact positive omission classification (#727). Decision 5 supersession reclaim implemented (see its implementation note); per-source shares and numbers remain the owner's. Broader end-to-end absence acceptance and decision 6 remain pending. Decision 7 (normalized receipt storage) slice 1 is implemented (see its implementation note); the later extraction slice is not. Decision 3 adds three indexes so the store's integrity checks stay linear in tombstones and reclaims, and binds each capture-forgotten receipt to the exact tombstone that marked it; saved work packets also pin their cited receipts (#749).
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
   - *Implementation note (#725).* Records live in the graph store beside the source bindings and describe the **current** graph, not each historical revision.
     - Each publish replaces a repo's records.
     - Each record carries the complete digest the omitted receipt was emitted for and is kept only when that equals the published fact's digest; a fact with any current binding is never recorded, and at selection a current binding from any repository overrides a surviving record. The fact triggers drop the record when the fact is rewritten.
     - Lookup is `FactKey → Omitted | NotOmitted | Unrecorded`. It is answered only inside the bounded source-selection read, in the same transaction and against the same copied graph digest as the association state, so a consumer never pairs an omission with a different revision. The selection returns its store revision, and `read_source_selection_snapshot_at_revision_bounded` refuses a changed basis. Omission tables are not generation-watched; publication and `clear` advance the revision themselves whenever they write or delete omission rows. The bounded selection preflights the coverage schema before decoding it, and omission cells are type- and length-checked in SQL before copying; a malformed row is never `Omitted`. There is no standalone unbounded lookup.

     This is equivalent to `(basis id, FactKey)` for the resolver, because step 4 runs only after the requested basis has been checked against the current graph; any other basis is already stale.

     Records are bounded at 262,144 per repo in the graph store rather than counted against receipt retention. Over the bound, or when a producer has only per-file counts, the repo records `incomplete`.
3. **Forget and pins.**
   - `forget` deletes capture bytes, as today. In the same transaction, under the retention guard, it reclaims exactly the receipt rows that the shared eligibility predicate (section 4) allows.
   - **Pinned receipts keep their metadata rows.** They are marked `capture_forgotten`, so a historical citation still resolves its identity and reports forgotten bytes instead of silently disappearing.
   - The tombstone records `(source id, source revision/basis, capture id, path, forgotten_at, receipts_reclaimed, receipts_retained_pinned)`.
4. **Absence resolution, scoped to exact identity.** In order:
   1. Try present evidence for the exact requested `(basis id, receipt id)` and its capture.
   2. Pinned receipt row with `capture_forgotten` → **forgotten**.
   3. Exact receipt id is a reclaimed row. A reclaim ledger keyed by receipt id records the tombstone it belongs to → **forgotten**.
   4. An exact current per-fact positive omission record → **omitted**. Incomplete or unrecorded coverage establishes neither a negative classification nor completeness.
   5. Otherwise → **missing**.

   Classification never uses the current source or path. A reimport gets a new source revision, so an old tombstone can't touch it, and an unknown or forged receipt id reaches step 5.

   Tests: forget then reimport, an old pinned receipt, an unrelated missing id, and simultaneous omission and tombstone records for the same fact. The receipt-id step wins because it is more specific.

   Complete-mode MCP checks the raw graph basis before classifying an unbound fact,
   then rechecks its revision, digest, omission and registration under retention
   protection. Positive records return `evidence_omitted` with `omission_kind`
   (`declarations`, `rules`, `store_capacity`); unknown or `NotOmitted` remains
   unavailable. A supplied receipt target whose binding disappeared remains stale.
   The partial omission and forgotten capabilities do not enable broad
   `evidence_absence_reason_available`; historical Inspector behavior is preserved.
5. **Reclaimable retention (shared eligibility predicate).**
   - A receipt row is reclaimable iff (its source is forgotten, or a newer completed recovery of the same source supersedes its capture) and no saved investigation ref, work packet or retained citation pins its receipt id.
   - Forget and supersession both use this one predicate, inside the committing transaction, under the retention guard, with a bounded pin query.
   - The global ceiling stays as the upper bound. Per-source shares and all numbers are proposed with measurements and are the owner's decision.
   - The `receipt-store-capacity` reason points at forget/supersession only once reclamation demonstrably frees space.
   - *Implementation note (#725).* Supersession runs after a recovery's graph publish. The job first releases its prepared input, then tries the source's exclusive retention guard, so no other recovery of that source can be holding receipts it has persisted but not yet bound. A busy source is skipped and settles on its next publish. Under that guard the kept set is the pins that forget uses plus the receipts of this source that the current graph binds. Every other unmarked row of the source is reclaimed into the ledger under a tombstone whose new `cause` is `superseded` (schema v3; existing tombstones are backfilled as `forgotten`). Superseded reclaims never mark pinned rows and leave capture-forgotten rows untouched. `ReceiptStatus::Superseded` is not "forgotten": an exact binding to it reads as unavailable or stale, so the MCP contract is unchanged. Capture bytes are not reclaimed by supersession.
6. **Extraction memory.** Receipts are admitted per file as they are produced, with no whole-repo intermediate vectors. The REA peak is a measured acceptance item.
7. **Normalized receipt storage** (proposed r1 for #725; Codex source review 6411550381 folded in). On REA at `258a1dd`, 37,025 kept receipts fill the 64 MiB extraction budget, but they share only 1,663 distinct per-file headers. Storing each header once, with compact bodies, measures 19.4 MiB instead of 64 MiB.
   - **Wire and identity are unchanged.** The store keeps a header row per distinct header and a body row per receipt. Canonical receipt bytes are rebuilt exactly from the two, so `receipt_id`, the hash domain and every existing verification check stay as they are. Nothing new is hashed.
   - **The header is the whole shared identity.** It holds every field the receipts of one file share: source id, repo key, capture id, path, digest, byte length, commit, producer contract, grammar and parser packages. Its key is the digest of its canonical bytes. The body holds the remaining fields plus the fact id with the `repo_key@path` prefix removed. A body is stored this way only if prefixing it again reproduces the fact id exactly; otherwise it is not normalized.
   - **An exact inverse, checked on every read.** A read rebuilds the canonical bytes, decodes them through the existing `Receipt::from_json` and requires the same id, source, repo and re-serialized bytes that `stored_receipt` requires today. A missing header, a header owned by another source or repo, and contradictory fields all fail closed as `Invalid`. Headers are never shared across sources.
   - **Bounded reads before allocation.** Header and body cells are type-, UTF-8- and length-checked in SQL, in one coherent read, before anything is copied. The rebuilt bytes are bounded by `MAX_RECEIPT_BYTES` before decoding, and the existing per-receipt expanded-range bounds still apply. Store validation and lookup never hydrate every receipt.
   - **Shared-header retention is transactional.** The store budget charges each retained header once plus every body. Deduplication, rollback and exact/one-below admission agree for normalized, legacy and mixed rows. Forget and supersession (decisions 3 and 5) delete a header only when no retained or pinned body references it, in the same transaction. Tombstones, the reclaim ledger and historical classification keep their meaning.
   - **Mixed formats.** Existing v1/v2/v3 rows stay readable as they are and are never re-identified. A schema migration only adds the two tables, and an interrupted migration rolls back to the previous schema.
   - **Slice 1 is storage-only. The extraction charge does not change.** Extraction still retains expanded `Receipt` values and persist still builds serialized rows, so charging stored bytes would admit more heap under the same apparent budget. Slice 1 therefore frees store capacity and changes nothing about how many receipts an extraction keeps. A later slice may introduce a bounded normalized extraction carrier and charge the header table, bodies, references and temporary reconstruction owners. Only that slice can raise the citable fraction, and it keeps rule-before-declaration admission and exact omission provenance.
   - **Numbers.** No ceiling changes here. At 3.3×, the current 64 MiB extraction budget would fit about 120k REA receipts by bytes after the later slice. The 100,000 count ceilings (`MAX_EXTRACTION_RECEIPTS` and the store's `MAX_RECEIPTS`) still stop an extraction at 100k. Full REA coverage would need about 85 MiB and a 160k receipt count, which are the owner's call.
   - Tests: byte-identical reconstruction and ids for v3 vectors; multi-source and multi-producer non-aliasing; corrupt, missing and cross-source headers; mixed-format reopen; interrupted migration; exact and one-below budget boundaries; and a shared header surviving the forget or supersession of one of its receipts while another body or pin references it.
   - **Implementation note (slice 1, #725, AC-0373).** Receipt store schema v4 adds a `receipt_headers` table and a nullable `receipts.header_id`; a NULL `header_id` marks a legacy row whose payload is the full canonical receipt. The header id is the content hash of `cartograph:receipt-header:v1\0` plus the header's canonical JSON. Only v3 receipts whose rebuild matches `to_json()` byte for byte at write time are normalized; everything else is stored as before. An exact v3 store migrates in the open transaction (the column is added with `ALTER TABLE`, which yields the same schema text as a fresh store). `receipt_storage` rejects a header no row references, so every delete path removes orphaned headers in its own transaction.

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
