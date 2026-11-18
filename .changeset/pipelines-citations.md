---
"cartograph": minor
---

`pipelines.md` now prints a fact-index citation id beside each statement it makes. Every pipeline, trigger, job and step row gains a Citation column with the id of its fact; a trigger, job or step also cites the relation that places it, and a job cites each of its needs in the order its Needs cell lists them, each relation with its confidence. Every id printed resolves to a row of `fact-index.md` and `fact-index.json`. A `verified-only` export still prints no InferredWeak fact and so no id for one; a `best-effort` export prints them with their confidence. The document's bytes and content identity change; no other artifact changes.
