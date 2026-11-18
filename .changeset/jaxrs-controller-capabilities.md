---
"cartograph": minor
---

Capabilities are now also recovered from JAX-RS resource classes in Java: each resource class becomes one capability candidate that cites the class, its HTTP endpoints and their handlers. Sub-resource locators and resource methods declared on an interface stay route gaps and give no candidate. Re-ingest a repository to get them. Flask, Go and Rust route groups are not covered yet.
