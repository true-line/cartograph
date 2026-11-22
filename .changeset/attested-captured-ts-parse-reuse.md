---
"cartograph": minor
---

Re-ingesting a repository no longer re-parses TypeScript and JavaScript files whose captured bytes are unchanged. Each reused file's source receipts are still minted from the current capture, so the graph and its citations match a full re-parse (#438, ADR-0087, AC-1517).
