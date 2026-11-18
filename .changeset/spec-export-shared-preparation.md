---
"cartograph": patch
---

Export bundle now fetches every artifact whose preview was capped in one request, so the graph snapshot, flow trace and curation are prepared once for all of them instead of once per artifact. The exported file is unchanged, and an incomplete or changed result still refuses the whole export.
