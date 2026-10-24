---
"cartograph": patch
---

Coverage: channel and client-call stitching now reports its own `t0.events` row in the ingest summary and the coverage inventory, instead of adding its facts to the TypeScript adapter's counts (#874). A repo without TypeScript, such as SimpleLogin Android, no longer shows a not-applicable TypeScript producer carrying 41 nodes and 41 edges.
