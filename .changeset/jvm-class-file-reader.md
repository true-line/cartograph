---
"cartograph": minor
---

Ingest now reads compiled JVM class files a repository ships without their source, as classes, methods, calls and imports. Each fact cites the class file and the bytes it came from, and they show as their own "JVM classes" layer. For a cloned repository the class files are read from the cloned commit, not the working copy. Scripts (`.kts`) count as Kotlin source. A file it cannot read, one with a malformed signature or one over 8 MiB, is recorded as a gap, and overloads whose parameter types share a short name keep separate symbols (#1445).
