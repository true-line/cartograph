---
"cartograph": minor
---

Whole-system ("Recovered system") investigations no longer load the whole graph. Preparation pins a store revision and streams only the graph's identity; each query reads one page from the store, so memory tracks the question, not the estate.
