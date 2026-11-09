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
| Finish shape clarification (existing limits, not permission to finish): overall limitations and each finding's limitations must contain 1..8 nonblank strings, each at most 512 UTF-8 bytes. Titles and statements must be nonblank, at most 160 and 2048 UTF-8 bytes respectively. No string may contain NUL. Each finding needs 1..8 unique exact admitted citation IDs, each nonblank and at most 128 UTF-8 bytes. Use at most 12 findings. Nonempty findings require knowledge_completeness partial; empty findings require insufficient_evidence. The outer outcome must agree. Do not invent findings or citations to satisfy shape constraints. | Finish shape (unchanged limits; not finish permission): overall/per-finding limitations need 1..8 nonblank strings. UTF-8 byte caps: each limitation 512, nonblank title 160, nonblank statement 2048, each citation ID 128. No NUL anywhere. Each finding needs 1..8 unique exact admitted nonblank citation IDs; at most 12 findings. Nonempty findings require knowledge_completeness partial; empty require insufficient_evidence; outer outcome must agree. Never invent findings/citations to meet shape. |
| The supported wrapper example above and this insufficient-evidence wrapper are the two terminal shapes. This example is syntax, not evidence or permission to finish before completing the required obligation attempts: | The supported example above and this insufficient-evidence example are the only two terminal shapes: syntax, not evidence or permission to finish before completing required obligation attempts: |
| Describe cited source in your own words; never copy a complete source excerpt into title, statement or limitations. | Paraphrase cited source; never copy a complete source excerpt into title, statement or limitations. |
| Historical correction actions in completion_metadata record the refused invocation; use only current correction entries and acquisition_choices for the next action. | Historical completion_metadata correction actions record the refused invocation; use only current correction entries and acquisition_choices next. |
| This is a multi-step host-executed protocol. The host-derived turn_progress describes this turn, not a new session. | Multi-step host-executed protocol: host-derived turn_progress describes this turn, not a new session. |

The JSON examples, tool/schema rules and guards remain unchanged. The regression
requires at least 32 bytes under the existing 16384-byte serialized definition cap.
The current definition is 16349 bytes (35 bytes of headroom). Table rows align
individual constraints; the compact paragraph combines their punctuation.
Only the following two generic rule lines are added:

Code-derived behaviour: say "the code shows"; say "the comment states" only for cited comment text.
Sentinels: name exact constants/values, never "unbounded", "infinite" or "unlimited" unless cited text says so.
