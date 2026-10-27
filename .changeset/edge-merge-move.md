---
"cartograph": minor
---

Graph load moves recovered facts into the publish instead of copying every edge and changed node, and receipt matching reads a borrowed view of the merged edges, lowering peak memory on large ingests (#1235).
