---
"cartograph": patch
---

Spec export no longer copies the graph to compile it: the projection borrows every selected fact and owns only the ADR, drift and security facts it derives, and typed provenance is parsed only for the facts ADR recovery and drift actually inspect. On the SimpleLogin showcase the export heap peak drops from 749 MB to 358 MB with the same artifacts (#725).
