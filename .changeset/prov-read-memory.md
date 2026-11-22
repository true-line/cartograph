---
"cartograph": patch
---

The spec export reads the recovered graph in less memory. Each fact's provenance and each business rule's payload is held as compact text as the graph is read, so the graph is never decoded whole. On a 5,360-file TypeScript repository the export's working memory fell by about 1.3 GB, and the exported bundle is unchanged.
