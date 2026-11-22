---
"cartograph": minor
---

Ingest now reads compiled JVM class files a repository ships without their source, as classes, methods, calls and imports. Each fact cites the class file and the bytes it came from, and they show as their own "JVM classes" layer. For a cloned repository the class files are read from the cloned commit, not the working copy. A file it cannot read is recorded as a gap (#1445).
