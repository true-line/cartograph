---
"cartograph": patch
---

Repository ingest keeps recovered facts packed after the TypeScript walk: later passes decode only the labels they read, so recovery holds far less memory on large repositories (#1574).
