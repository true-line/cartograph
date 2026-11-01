---
"cartograph": patch
---

Job-store connections now wait up to 5 seconds (was 1 second) for a competing writer before reporting a storage error. This reduces spurious storage failures when app and MCP requests contend on a busy machine, for example concurrent identical MCP retries. Contention beyond 5 seconds can still fail.
