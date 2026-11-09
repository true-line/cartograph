# #598 pre-run amendment v1 — analyst attribution and sentinels

Owner decision: 2026-11-09, amend plus one new attempt.

Prior run: `598-v066-20261109`, receipt SHA256
`d7b9125799c2e2698d97fc1fc778d4ce5231258f207d35112bf0243dbccbb515`.
Independent semantic review: [comment 6732330552](https://github.com/true-line/cartograph/issues/598#issuecomment-6732330552), **FAIL**.

The two failure classes were attributing inferred code behaviour to a comment
that did not state it, and describing a finite sentinel as unbounded.
The analyst criterion passed; semantic review failed. The attempt is preserved.

The proposed next version is `domain-analyst@28`, the next unused registry ID.
It shortens @28's inherited finish-shape explanation without changing its
meaning, then adds only generic attribution and exact-sentinel instructions. No target facts, rubric or oracle text enter the prompt. Desktop/default
selection moves to @28; @27 remains reproducible. Correction, privacy and all
limits, including LocalFinish4096V1, remain unchanged.

Frozen rubric SHA256 remains
`75a1c9e7543ab40fde971f738aa86f53a8c59bf68ba89bf4f578893c998a3450`.
This amendment does not weaken that rubric, change the oracle, or rehabilitate
the failed receipt. No auditor, negatives, native or retention stages follow the
failed attempt. The new acceptance attempt requires a fresh sealed packet and
root; `/Users/user/cartograph-acceptance/598-v066` is not a development root.
Local loopback-only development runs use a separate development root and are
not acceptance evidence. No hosted development runs are authorized.

## Wording compaction for review (@28 only)

| @27 wording (unchanged) | @28 wording |
| --- | --- |
| Finish shape clarification (existing limits, not permission to finish): | Finish shape (same limits; not finish permission): |
| overall limitations and each finding's limitations must contain 1..8 nonblank strings, each at most 512 UTF-8 bytes. | overall/per-finding limitations: 1..8 nonblank strings. UTF-8 byte caps: limitation 512 |
| Titles and statements must be nonblank, at most 160 and 2048 UTF-8 bytes respectively. | title 160, statement 2048; Titles/statements/IDs nonblank |
| No string may contain NUL. | no NUL |
| Each finding needs 1..8 unique exact admitted citation IDs, each nonblank and at most 128 UTF-8 bytes. | citation ID 128; IDs nonblank; Per finding: 1..8 unique exact admitted citation IDs. |
| Use at most 12 findings. | At most 12 findings. |
| Nonempty findings require knowledge_completeness partial; empty findings require insufficient_evidence. The outer outcome must agree. | knowledge_completeness: partial with findings, insufficient_evidence without; outer outcome agrees. |
| Do not invent findings or citations to satisfy shape constraints. | Never invent findings/citations for shape. |
| The supported wrapper example above and this insufficient-evidence wrapper are the two terminal shapes. This example is syntax, not evidence or permission to finish before completing the required obligation attempts: | Both terminal wrapper examples are syntax, not evidence or permission to finish before required obligation attempts complete: |

The JSON examples, tool/schema rules and guards remain unchanged. The regression
requires at least 64 bytes under the existing 16384-byte serialized definition cap.
The current definition is 16305 bytes (79 bytes of headroom). Table rows align
individual constraints; the compact paragraph combines their punctuation.
Only the following two generic rule lines are added:

For behaviour inferred from code structure, say "the code shows"; "the comment states" only for cited comment text.
Sentinels: name exact constants/values, never "unbounded", "infinite" or "unlimited" unless the cited span literally says so.
