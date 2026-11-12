---
"cartograph": patch
---

An MCP read of `coverage` or `coverage-index` now returns the stored coverage inventories, the same content as the exported `coverage.md` and `coverage.json`, instead of saying that no inventory was supplied; a repository whose inventory was recorded under a registration or plugin basis that is no longer current is listed as stale with its inventory hash, and nothing is read from that inventory.
