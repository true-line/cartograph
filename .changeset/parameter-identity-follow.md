---
"cartograph": minor
---

A Python client whose identity is a function parameter is now followed to the function's one static caller (and on through `config.X`), so SimpleLogin's Redis rate-limit storage resolves to `MEM_STORE_URI` instead of a Gap. The result is InferredStrong and marked `identity_assumes_static_callers`.
