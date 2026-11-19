---
"cartograph": minor
---

Capabilities are now also recovered from Flask Blueprints: the routes a Blueprint serves from one file become one capability candidate that cites each endpoint and its handler function. Routes registered on the application itself belong to no group. Re-ingest a repository to get them.
