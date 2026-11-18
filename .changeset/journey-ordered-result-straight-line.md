---
"cartograph": patch
---

A journey candidate no longer shows a result screen as following its goal call when the call is made only on some paths, such as inside an `if`, a loop, a `try` or a callback, and the navigation after it is not. Such a journey is now listed with an unordered result. The check uses a new record on each TypeScript or JavaScript client call, so a system ingested before this release lists every result as unordered until it is ingested again.
