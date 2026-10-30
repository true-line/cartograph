---
"cartograph": patch
---

Adds the knowledge-layer embedding phase from ADR-0051 as a library step. It picks graph nodes by a versioned recipe, redacts their text before it reaches the local embedding model, and reuses cached vectors for unchanged nodes. With no embedding model it reports a named omission and leaves the base graph untouched (#1441).
