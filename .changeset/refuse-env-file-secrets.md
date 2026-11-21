---
"cartograph": minor
---

Ingest now refuses a checkout whose `.env` or `.env.*` files hold secret-shaped values (recognized secret patterns, token-shaped values, URLs with a password, or real values under credential key names). The refusal names each file, line and key, never the value, and says how to proceed: remove the values or files, and declare non-secret values the resolver needs in `cartograph.system.toml` under `[env]`. Previously such values could reach the graph and exports.
