---
"cartograph": minor
---

A scheduled job's dispatched branch can now call a function imported from another module in the repository, or run a coroutine through `asyncio.run`, and its flow still starts at that function (#779).
