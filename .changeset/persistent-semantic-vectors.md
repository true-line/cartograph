---
"cartograph": minor
---

Reuse bounded local embedding vectors across semantic previews and app restarts. Model identity and redacted input changes invalidate reuse, while graph membership and evaluation decisions are recomputed for every preview. Clearing the graph or forgetting retained sources deletes cached vectors and invalidates in-flight cache work.
