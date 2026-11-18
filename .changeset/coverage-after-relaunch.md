---
"cartograph": patch
---

Workspace keeps its coverage inventory after you relaunch the app. The inventories earlier ingests recorded are read back from the store without re-running an ingest: a current one is shown as recorded, a stale one (its registration or plugins changed since) is named as stale, and a failed read says so instead of showing old cards. Clearing the graph removes them for good.
