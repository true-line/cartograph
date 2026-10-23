---
"cartograph": minor
---

Python code that reaches Redis through the `limits` rate-limit storages (`RedisStorage`, `RedisSentinelStorage`, `RedisClusterStorage`) now shows an observed `redis` resource, or a Gap naming the call when the URL is computed at run time (#734).
