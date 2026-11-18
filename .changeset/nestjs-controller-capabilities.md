---
"cartograph": minor
---

Capabilities are now also recovered from NestJS REST controllers: each controller class becomes one capability candidate that cites its HTTP endpoints and their handlers. Until now only NestJS GraphQL resolvers produced capabilities. Re-ingest a repository to get them. Other frameworks' controllers are not covered yet.
