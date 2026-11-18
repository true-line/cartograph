---
"cartograph": patch
---

Found ADR and RFC files are now redacted before their text is stored. A secret pasted into a decision record no longer reaches the graph or the exported `adrs.md`; the fact records how many values were replaced. An ADR file over 1 MiB or not valid UTF-8 is skipped instead of failing the whole ADR read, and the coverage inventory names each skipped file by reason. The ADR reader and the API contract reader now have their own coverage rows.
