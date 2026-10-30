---
"cartograph": minor
---

Python ingest reads `aiohttp` client requests as fetch sites: `aiohttp.request(method, url)` and the verb methods of a once-bound `aiohttp.ClientSession`, whose `base_url` is resolved against as aiohttp does (#1432).
