---
"cartograph": patch
---

Adds the knowledge overlay store from ADR-0051. Derived knowledge-layer facts live in a separate, disposable file, and each fact cites the base facts it rests on. Facts are read back only at the base revision they were derived from. The overlay is discarded together with cached vectors whenever the graph is cleared or a retained source is forgotten (#1440).
