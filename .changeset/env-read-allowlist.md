---
"cartograph": minor
---

**Behavior change:** the config resolver no longer reads env-file values implicitly. It reads a value only for a key listed in `env_read` in `cartograph.system.toml` or in `read` in a repository's `.cartograph/env.toml` (exact names, no globs). A channel or client call whose key an env file sets but no list names is now a Gap that names the key, the file and how to list it. URL values lose any `user@` part; `[env]` values and the secret refusal are unchanged (#933, ADR-0075).
