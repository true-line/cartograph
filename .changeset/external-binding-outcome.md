---
"cartograph": minor
---

In a system with declared bindings, `external.md` now lists a call to a host no binding names as outside, with its call sites, spans and flows, where it was listed as unclassified before. Adding a system records, on each outbound call it could not join to an endpoint, whether a declared service or gateway route names the call's host, and records the bindings on each observed HTTP client of a declared host. A call that a binding places in an ingested repository and that matched no endpoint is still counted and not listed, including a call whose method is computed, and an observed HTTP client of a declared host is no longer listed as outside. A system added before this release keeps the earlier result until it is added again. The recorded outcome changes those facts, so the graph hash of a system with declared bindings changes on its next ingest.
