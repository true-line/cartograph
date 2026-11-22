---
"cartograph": patch
---

Spec exports and ingests of very large repositories use less memory to compute the graph content hash, and the gap register no longer holds two copies of every row's provenance. On a 16,180-file TypeScript repository the hash step's extra memory fell from 1.5 GB to 1.0 GB, and every exported file is unchanged.
