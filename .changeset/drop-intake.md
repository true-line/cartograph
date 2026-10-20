---
"cartograph": minor
---

Folders can be dropped onto the Connect step. Each dropped folder ingests the same way as a typed local path, or as a system when it holds a system manifest. Every path then shows its own outcome: rejected with a reason, ingested with each repo and its commit identity, or failed with the error. The set is never reported as fully ingested while any folder is still running, rejected or failed, and a local ingest's summary now names its repo.
