---
"cartograph": minor
---

Resolving an unresolved call now cites captured source for every piece of evidence: the call's Gap, its file and its tests get primary-source receipts, so a task no longer mixes in unverified working-tree reads. A captured span too large for the task is left out and named, instead of failing the task.
