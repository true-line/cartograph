---
"cartograph": minor
---

A gated plugin that fails on one file (a malformed fact, a guest error or trap, or its fuel, memory or table bound) no longer fails the whole ingest: that file is skipped for that plugin and no fact from it by that plugin joins the graph. The coverage inventory, `coverage.md`, `coverage.json` and the Workspace name the plugin, the refused files and the reason class, and do not count those files as covered; a plugin that runs past its time limit still fails the ingest.
