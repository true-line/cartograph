---
"cartograph": minor
---

Recovering a source again now reclaims the receipts its previous recovery left behind. After a recovery publishes its graph, every receipt of that source that nothing pins and the current graph no longer binds is reclaimed under a `superseded` tombstone, so the receipt store stops growing with each re-import. Pinned and capture-forgotten receipts, capture bytes, and the receipts of a task prepared during the reclaim are kept (#725, ADR-0046 decision 5).
