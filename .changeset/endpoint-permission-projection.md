---
"cartograph": minor
---

Every export now includes `endpoint-permissions.md`. It lists each recovered endpoint with the permission sites recovered for it, the endpoints with none recovered, and the endpoints whose handler path could not be read to its end, each with tier, confidence and source span. "None recovered" means only that: a check made by middleware, a gateway or code inside a handler is not recovered, and the document never says an endpoint has no check. Permission sites are recovered for Flask, Flask-Admin and NestJS GraphQL today; an endpoint in any other framework is listed under a coverage note and in neither table. A `verified-only` export leaves out a row whose permission site is InferredWeak and states how many; a `best-effort` export marks it. The document can be read over MCP and selected in a work packet like the other graph documents.
