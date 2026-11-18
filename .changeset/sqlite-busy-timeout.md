---
"cartograph": patch
---

The source-capture store and the primary-source receipt store now wait up to 5 seconds for another connection's write lock, like every other local store, instead of 1 second. On a loaded machine a recovery or evidence read is less likely to fail with "database is locked" while another window or the MCP server is writing.
