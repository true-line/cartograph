---
"cartograph": patch
---

Publish recovered facts packed: ingest holds each fact as compact JSON from merge through the conditional graph write, decoding only the facts it needs to compare, so publishing a large repository uses less memory with identical rows.
