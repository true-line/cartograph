---
"cartograph": patch
---

The knowledge phase that follows each ingest reads the recovered graph in less memory. Each fact's provenance is held as compact text as the graph is read, so the graph is never decoded whole. On a 5,360-file TypeScript repository the phase's working memory fell by about 1.1 GB, and the derived facts are unchanged.
