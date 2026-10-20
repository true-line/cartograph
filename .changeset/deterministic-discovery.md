---
"cartograph": minor
---

The context hub can now find nodes deterministically, by exact identifier, exact name or case-insensitive text, under a declared and versioned matching policy (`discovery-v1`). Each candidate is a bounded handle that carries its label, its source repo, path and commit, why it matched, its edge counts and its confidence. Same-name nodes in different repos stay separate candidates and are never merged or bound. Every response states what was examined and what the policy does not search, so an empty page never reads as "nothing exists". Continuation cursors are bound to the snapshot, normalized query and policy version, and a candidate too large for the byte budget fails explicitly instead of being skipped.
