# ADR-0086 — Spec bundle file v2: each piece once

- **Status:** Accepted (coordinator as the owner's delegate, 2026-11-22)
- **Date:** 2026-11-22
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #1582 (bundle size); #2215 (export memory, closed); AC-0740
  (host-written bundle file); AC-0218 (complete, untruncated export);
  AC-1212 and AC-1213 (streamed and renamed bundle file); ADR-0058 (fact
  index)
- **Supersedes:** AC-0740's promise that the file is byte for byte the JSON
  the Workbench built before #1582

## Context

Export bundle writes one JSON file: the preview's keys, every artifact in
full, then `files` mapping each file name to its content, indented by two
spaces (AC-0740). AC-0740 pins that file byte for byte to what the Workbench
built before #1582, so the host and the browser preview write the same bytes.

The file is now the problem. On VSCode (16,180 files, 1.28M nodes, 2.03M
edges) it is 7.67 GB. The REA field measurements on #1582 show where the
bytes go:

- **Every artifact's content appears twice:** once in `artifacts[].content`
  and again in `files`. On VSCode that content is 2.23 GB.
- **Indentation:** two-space pretty printing adds about 22% over compact
  JSON (912 MB against 713 MB on REA).
- **Assertions are most of the rest** (593 of 713 compact MB on REA):
  - `id` is always `node:` or `edge:` plus `subject_id`, so it repeats
    `subject_id` (113.7 MB).
  - Every evidence row repeats its `repo` and `commit_sha` (47.5 MB).
  - 181,202 of 552,252 assertions are exact duplicates that a second
    artifact cites (the gap register and rule evidence share 155,026).

No product path reads a bundle file back. The file is a handoff deliverable
read by people, by scripts, and by the agents a handoff feeds.

## Decision

1. **A versioned file.** A v2 file opens with
   `"schema": "cartograph.spec-bundle-file/v2"`. A file without `schema` is
   v1 (the AC-0740 shape).
2. **Content once.** An artifact in `artifacts` no longer carries `content`.
   `files` maps each artifact's `file_name` to its content and is the only
   place content appears. The official catalog's file names are unique, and
   a test pins that, so no content is lost to a repeated name. Each artifact
   keeps its `content_id`, which still identifies its content in `files`.
3. **A source table.** The bundle lists each distinct evidence source once,
   in `sources`, as `{ "repo", "commit_sha" }` in first-use order. An
   evidence row is `{ "source": <index>, "path", "byte_start", "byte_end" }`.
4. **Derived assertion ids.** An assertion whose `id` is `node:` or `edge:`
   followed by its `subject_id` writes `"ref": "node"` or `"ref": "edge"` in
   place of `id`. Any other id (a flow hop's, a synthesized one) is written
   as it is.
5. **Shared assertions once.** The bundle lists each distinct assertion once,
   in `assertions`, in first-use order. Each artifact's `assertions` is the
   list of indexes into that table, in the artifact's own order, so every
   artifact still lists exactly the assertions it had.
6. **Compact, one entry per line.** The file is compact JSON with a newline
   after each top-level key and after each entry of `artifacts`, `sources`
   and `assertions`. It diffs by line and greps, without pretty printing's
   cost.
7. **Top-level order:** `schema`, `mode`, the counts, the bases, `sources`,
   `artifacts`, `assertions`, `files`.
   - The host writes the file as the artifacts compile (AC-1212).
   - Artifacts, the assertion table and the content spool are written to
     their own temp files, then joined.
   - The source table and the dedup index are the only state held across
     artifacts: one entry per distinct source, and a digest per distinct
     assertion.
8. **v1 stays readable.**
   - The spec crate gains one reader that takes a v1 or a v2 file and
     returns the same `SpecBundle`: every artifact with its content and its
     full assertions.
   - A v1 fixture and its v2 counterpart must read to equal bundles, and a
     v2 file must read back to the bundle it was written from.
   - The reader is the documented way for a script or agent to load either
     version.
9. **One writer shape in two places.** The browser preview builds the same
   v2 file in TypeScript (`specBundleFileContent`). Its fixture
   (`spec-bundle-file.expected.json`) becomes the v2 file, and the host's
   streamed file must equal it byte for byte, as AC-0740 required of v1.
   The v1 fixture is kept as reader input.
10. **What AC-0740 now promises.** The host and the browser write the same
    bytes for the same preview, and the bundle is complete (AC-0218). The
    file is no longer byte for byte the pre-#1582 Workbench JSON. That
    promise is withdrawn.
11. **A recorded size budget.** The bundle is never truncated (AC-0218), so
    there is no omission to name in the file itself. Instead:
    - A fixture test bounds v2's bytes per assertion and per content byte
      against v1's.
    - REA and VSCode sizes for v1 and v2 are recorded in the PR that turns
      v2 on, and in the US-TM row of its acceptance criterion.
    - A later regression past the budget fails that test, not the export.

## Consequences

- **Smaller file.** Dropping the duplicate content alone takes VSCode's file
  down by about 2.2 GB. Applying REA's measured shares for indentation, ids,
  sources and duplicates to the rest is an estimate of roughly 60% smaller
  in all. The PR that turns v2 on records the real numbers.
- **Breaking for consumers that parse v1 directly.** A consumer that reads
  `artifacts[].content` or `assertions[].provenance.evidence[].repo` needs
  the reader or the v2 shape. Release notes say so.
- **Memory is not the goal.** Compile memory does not change with the file
  format: the host still compiles every assertion. The dedup index adds one
  digest per distinct assertion (about 50 MB on VSCode).
- **Landing in small PRs, each green and shippable:**
  1. This ADR. AC-0740 is amended in step 3, when the file it describes
     changes.
  2. The spec crate's v2 types, writer and v1/v2 reader, with fixtures and
     the size test, not yet used by the export.
  3. The host streams v2, and the UI preview builds v2, with the fixtures
     updated together.
  4. REA and VSCode measurements, and the recorded budget.

## Alternatives

1. **Keep v1 and only compress the file** (gzip the saved bundle). This
   shrinks the download but not what a reader has to parse or hold, and it
   leaves every duplicate in place. It can still be added on top of v2.
2. **One file per artifact plus a small index** instead of one bundle file.
   This is closer to a docs tree, but the save panel picks one path. It would
   also split one export into many writes with no single atomic replace, and
   it gives up the one-file handoff that AC-0740 and its consumers rely on.
3. **Drop `files` and keep content inline in `artifacts`.** This saves the
   same 2.2 GB, but `files` is the shape a consumer uses to lay the documents
   out on disk, and inline content keeps each artifact's metadata line
   gigabytes long.
