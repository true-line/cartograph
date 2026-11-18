---
"cartograph": patch
---

The real-repository determinism check now names the first differing fact when two ingests of the same checkout disagree: a node or edge present in only one ingest, or present in both with different content. Before, a failure said only that the graph changed.
