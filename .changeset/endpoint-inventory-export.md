---
"cartograph": minor
---

Every export now includes `endpoint-inventory.md`. It lists every recovered endpoint once, including endpoints no test references, with its method, path, framework and language as the adapter recorded them, shown as `unknown` where nothing was recorded, and with the endpoint's tier, confidence and source span. It states no parameters, response schemas or authentication. An endpoint recorded as a Gap is counted and not listed. A `verified-only` export leaves out InferredWeak endpoints and states how many; a `best-effort` export marks them.
