---
"cartograph": minor
---

Ingest now reads compiled JVM class files a repository ships without their source, as classes, methods, calls and imports. Each fact cites the class file and the bytes it came from, and they show as their own "JVM classes" layer. A file it cannot read is recorded as a gap (#1445).
