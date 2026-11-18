---
"cartograph": patch
---

Semantic retrieval now takes its vector candidates from a usearch index that is rebuilt from the cached unit vectors. The index only proposes candidates: scores and order still come from the stored vectors, so results on small graphs are unchanged.
