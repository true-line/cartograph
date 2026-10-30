---
"cartograph": patch
---

Adds graph-first hybrid retrieval from ADR-0051 as a library step. A query that names a symbol or endpoint returns that node first, then its graph neighbours, then text matches ranked by SQLite full-text search and, when a local embedding model is available, fused with vector similarity. Every hit cites the base graph and nothing is written back (#1442).
