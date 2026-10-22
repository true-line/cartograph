---
"cartograph": minor
---

Recover documented API contract drift from author-declared Markdown and OpenAPI 3 JSON references (`.cartograph/api-docs.toml` and `cartograph.system.toml` `api_docs`), comparing canonicalized `DocumentedRoute` facts against recovered `Endpoint` nodes and reporting `documented-not-served`, `served-undocumented`, and `method-mismatch` findings with both sides cited in `drift_register.md` (#739).
