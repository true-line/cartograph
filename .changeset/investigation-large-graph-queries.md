---
"cartograph": patch
---

Agent drafting no longer fails on large codebases. A whole-system query that matched more than 50,000 facts (SimpleLogin matches 84,625) was rejected by the investigation ledger, so every section-writer run stopped at its first query with "Investigation input preparation failed". Only the returned page and the cursor are bounded now.
