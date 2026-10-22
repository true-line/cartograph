---
"cartograph": patch
---

Large TypeScript repositories no longer fail to recover when their primary receipts exceed the extraction budget or the retained receipt store is full. Receipts that fit are kept in a stable order, facts keep their place in the graph, and coverage names each file whose facts could not be cited, with a separate entry when the receipt store is full.
