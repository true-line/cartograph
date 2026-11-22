# ADR-0087 — Attested reuse of captured TS parses

- **Status:** Accepted (coordinator as the owner's delegate, 2026-11-22)
- **Date:** 2026-11-22
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #438; #243 (option 3); ADR-0024 (amends the decision to
  disable participating TS cache reuse); SPEC-06; AC-0040 (delta
  re-ingest); AC-0148 (the capture is the producer input); AC-1517

## Context

ADR-0024 made the production TypeScript lane parse the retained capture
buffer and mint per-fact receipts from that parse. It disabled TS parse-cache
reuse on that lane "until cache attestation is implemented". The ordinary
`IncrementalCache` records only a source hash next to the facts, and a source
hash from a live read says nothing about the bytes the receipts cite. Every
captured ingest therefore parses every TS/JS file again. On large
repositories that parse is the dominant cost of a recovery.

A receipt binds four things: the complete emitted fact, the original ranges
in the retained capture, the registered source identity, and the
producer/grammar identity. The parse produces the first. Minting produces the
other three, from the parse and the capture.

## Decision

1. **The producer itself attests its own parses.** For each captured file it
   parses, the TS lane records an attestation next to the parse:
   - the producer contract (crate version plus a parse-contract suffix);
   - the effective registry set identity;
   - the registered source id and repository key;
   - the file's path, full raw-byte digest and length, as the retained
     capture binds them.

   The parse is stored packed: the extraction, and the direct facts that
   receipts are minted from. Only the producer module can build or read
   these entries. Nothing decodes one from outside, so a caller cannot
   supply a parse for the cache to vouch for.
2. **Reuse requires every attested field to be equal.** On the next captured
   walk, a file reuses the cached parse only when its attestation is equal
   in every field. Any difference, including a missing entry, parses the
   retained buffer again.
   - The commit is not attested. A parse made at another commit has its
     evidence retargeted to the current commit, exactly as AC-0040's
     ordinary cache does. A test holds that the result equals a cold parse
     at that commit.
   - The grammar is bound by the producer contract and by the path, since
     `.ts` selects the TypeScript grammar and other extensions the TSX one.
     This holds because the cache lives only in process memory.
3. **Receipts are always minted fresh.** Reused or not, every receipt is
   minted over the current capture's retained buffer and file binding. So a
   reused file's receipts are byte-identical to a cold walk's, and their
   ranges are checked against the bytes now retained.
4. **Directory completion and receipt matching run every time, unchanged.**
   A changed file can still affect the facts of its dependents.
5. **The cache never outlives the process or survives a failed walk.**
   - It is held per repository in the app's extraction caches, next to the
     ordinary ones.
   - Clearing the graph clears it.
   - A walk takes the cache whole and leaves it holding exactly that walk's
     parses. A failed walk leaves it empty.
   - Files gone from the capture are counted as deleted.

## Consequences

- Re-ingesting an unchanged capture parses no TS file. The graph, the
  receipts and the receipt omissions equal a cold run's.
- A one-byte change parses exactly that file.
- The delta summary reports reused and recomputed files for the captured
  lane, as it already does for the ordinary lane.
- Memory grows by one packed parse per captured TS file per repository, the
  same order as the ordinary cache.
- A persisted cache is not covered here. It would need an explicit grammar
  digest and an integrity check on load, so it needs its own ADR.

## Alternatives

- **Reuse minted receipts.** Rejected: a receipt names its capture, so
  receipts from an earlier capture cannot cite the current one.
- **Key on the live source hash, as the ordinary cache does.** Rejected: it
  does not bind the retained bytes the receipts cite (ADR-0024).
- **Include the commit in the attestation.** Rejected: reuse would then be
  limited to re-ingests of one commit. Commit retargeting is already the
  ordinary cache's contract and is tested against a cold parse.
