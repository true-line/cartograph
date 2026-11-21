---
"cartograph": patch
---

Curated reads no longer fail at 257 accepted section drafts or findings. Accepted drafts and findings are now bounded by their total size (64 MiB of drafts, 16 MiB of findings, the same worst case as before), so many small ones are all read; past that, `curated.md` states `accepted_too_large`.
