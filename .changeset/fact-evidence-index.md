---
"cartograph": minor
---

Every export now includes `fact-index.md` and `fact-index.json`: one row for each fact the exported documents state, with its tier, confidence, producing method, evidence basis, extractor, every evidence span and the documents that state it. Each fact has a stable citation id (`F-` and 16 hex characters) derived from the fact alone, so it is the same in every document and every export of the same graph. A `verified-only` export has no InferredWeak row, a `best-effort` export marks them, and gaps are marked as gaps. An export is refused if a cited id has no row or two facts share an id. The existing documents do not print the id beside each statement yet.
