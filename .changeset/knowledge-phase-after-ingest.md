---
"cartograph": minor
---

After an ingest finishes, Cartograph now runs the knowledge embedding phase in the background. When the local Ollama embedding model is installed it embeds the new graph and records the run in the knowledge overlay; without it, the run is reported as skipped and the ingest is unaffected (#1441).
