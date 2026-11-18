# ADR-0062 — A plugin failure on one file refuses that file, and coverage records it

- **Status:** Proposed; owner review
- **Date:** 2026-11-12
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1810; #1756; ADR-0017; US-0017 / AC-0070, AC-0594, AC-0658; R-INT-2

## Context

A gated plugin (ADR-0017) extracts one file per call. Any `HostError` from one
call failed the whole ingest before coverage was built. One malformed fact in
one file therefore stopped the ingest of every repository, and the reader saw
a failed ingest, not a coverage row naming the plugin and the file. #1756
asked for the rejection to be counted; there was no path from the host
failure to the coverage inventory.

The determinism invariant constrains the answer: re-ingesting the same commit
under the same registration must give an identical graph. Skipping a file is
only allowed when the skip itself is a function of the plugin, the file and
the limits.

## Decision

1. **Refusal.** A call that fails with a malformed fact, a guest extraction
   error, a wasm trap code, or an exceeded fuel, memory or table bound refuses
   that one file for that plugin. No fact from the call joins the graph and
   the pass continues.
2. **Failure.** The wall-clock deadline, a call-stack overflow (its depth
   depends on the host's native frames), compilation, instantiation, I/O and
   any call failure without a trap code keep failing the ingest. They depend
   on the machine, are not about one file, or have no established cause.
3. **Record.** The plugin's producer row in the coverage inventory gains an
   optional `refusals` object: exact counts for three classes
   (`malformed_fact`, `guest_error`, `bound_exceeded`), the refused paths
   sorted, at most 256 paths and 32 KiB of path bytes per plugin, and an
   `omitted` count. Guest error text is not stored. Refused files are not in
   the plugin's file count. Each class with a refusal also adds an
   `unsupported` entry, so the inventory is `limited`.
4. **Compatibility.** The inventory stays `coverage@1`. The field is absent
   when nothing was refused, so such an inventory keeps its bytes and hash,
   and stored inventories without the field still read.
5. **Surfaces.** `coverage.md`, `coverage.json` and the Workspace coverage
   view list the refused files as not covered by that plugin.

## Consequences

- An ingest that used to fail on a plugin fault now succeeds with a limited
  inventory. The graph lacks that plugin's facts for the refused files; the
  documents say so.
- A file near the fuel bound can meet the wall-clock deadline first on a slow
  machine. The result is then a failed ingest, never a different graph.
- The bound classes are deterministic for one application build. A build that
  changes a limit or the runtime can change which files are refused, as any
  extractor change can.
- A deterministic guest fault that surfaces without a trap code (for example
  an invalid string returned across the boundary) still fails the ingest.

## Alternatives

1. **Keep failing the ingest and only report the file.** Simpler, but one
   plugin fault still withholds every repository's documents.
2. **Disable the whole plugin on its first failure.** Loses the files the
   plugin reads correctly and hides how many files are affected.
3. **Refuse on the deadline as well.** Makes the graph depend on machine
   speed and breaks the determinism invariant.
