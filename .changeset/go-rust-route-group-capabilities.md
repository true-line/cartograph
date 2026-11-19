---
"cartograph": minor
---

Capabilities are now also recovered from Go and Rust HTTP routes: handler functions in one Go package (`net/http`, chi, gin) or one Rust module (axum, actix-web) become one capability candidate per file that cites each endpoint and its handler. These frameworks give a group of routes a path prefix and no name, so the package or module is the group. Re-ingest a repository to get them.
