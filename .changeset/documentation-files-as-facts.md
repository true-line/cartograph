---
"cartograph": minor
---

Reads a source's existing documentation as evidence. Ingest now stores each Markdown and plain-text file of a registered source as a `Document` fact with one `DocSection` fact per section, carrying its heading, level and byte span. Section text is not stored and headings are redacted. Files over 1 MiB or not UTF-8 are counted and skipped, and a re-ingest drops the facts of a removed file or section.
