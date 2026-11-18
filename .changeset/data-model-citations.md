---
"cartograph": minor
---

`data_model.md` now prints a fact-index citation id beside each statement it makes. A new table lists each entity and each node related to one, with its tier, confidence and citation id. The access and mapping relations table gains Confidence and Citation columns, and each relation field cites the relation it belongs to. Every id printed resolves to a row of `fact-index.md` and `fact-index.json`. A `verified-only` export still prints no InferredWeak fact and so no id for one; a `best-effort` export prints them with their confidence. The diagram is unchanged and carries no ids. The document's bytes and content identity change; no other artifact changes.
