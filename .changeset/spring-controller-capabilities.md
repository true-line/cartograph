---
"cartograph": minor
---

Capabilities are now also recovered from Spring MVC controllers in Java and Kotlin: each controller class becomes one capability candidate that cites the class, its HTTP endpoints and their handlers. NestJS controllers keep theirs under the same rule, now named `knowledge.capability.http-controller`. Re-ingest a repository to get them. JAX-RS, Flask, Go and Rust route groups are not covered yet.
