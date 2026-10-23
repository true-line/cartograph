---
"cartograph": patch
---

Registering sources concurrently no longer fails as "storage unavailable" when another registration's commit is slow; the registry now waits up to 5 s for it (#839).
