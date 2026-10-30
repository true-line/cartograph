---
"cartograph": minor
---

The receipt store shares its capacity across retained sources: one source's recovery keeps receipts up to a ceiling proportional to its captured bytes (never below a sixteenth of the store), and anything past it becomes a named omission rather than crowding other sources out (#725).
