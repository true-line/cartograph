---
"cartograph": patch
---

Prepares the curated projection for investigation findings (ADR-0074). The projection can now hold a finding accepted as curated, with a state of `eligible`, `stale` or `unresolvable` decided from the facts the finding cites. No screen accepts or shows one yet. The curated revision moves to `curated-revision-v3`, so every project reports a new curated revision once, with unchanged content.
