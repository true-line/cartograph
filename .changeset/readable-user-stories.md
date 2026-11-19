---
"cartograph": patch
---

`user_stories.md` now reads as a document. Each derived Capability lists its stories and the source files and handlers that implement them, and the caller permission sets are one table (permissions, operation, where the guard is) instead of one JSON block per operation. The exact story identities and cited facts are kept, as JSON, in a "Story identities and support" appendix at the end. On Vendure `packages/core` the readable part is now about 88 KB instead of being spread through 2.1 MB of JSON.
