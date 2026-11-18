---
"cartograph": patch
---

Saving a follow-up decision while another window or the MCP server is writing to the same store no longer reports a storage failure when the wait for that writer runs out. The decision is reported as not saved, and you are asked to refresh the reviews and choose again. Nothing is retried automatically.
