---
"cartograph": minor
---

Semantic previews now handle VSCode-scale repositories in one pass: up to about 262,000 candidate and query texts are embedded, cached and reused, where the cache previously stopped at 100,000 and re-embedded the rest on every run.
