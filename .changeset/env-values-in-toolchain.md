---
"cartograph": minor
---

Allowlisted env values now appear in toolchain facts: an env file's tool lists the values of the keys named in `env_read` / `.cartograph/env.toml`, each citing its own line, and opening that evidence shows the line alone. Template files (`.env.example`, `env.sample`, `.env.dist`) are marked as templates, and the config resolver now prefers any real env file over a template (#933, ADR-0075).
