---
"cartograph": minor
---

Recover client HTTP calls declared as data: Retrofit interface annotations in Kotlin and Java, joined to the repository's `Retrofit.Builder().baseUrl(...)`, and `{ method, path }` route tables in JavaScript and Vue, joined to their base URL by a wrapper or a direct `fetch`. A base read from runtime state resolves only when `cartograph.system.toml` declares it under `env`, so joined calls can bind to another repo's endpoints; anything unresolved stays a Gap naming the entry. Flask `<id>` and FastAPI `{id}` route parameters now match client paths like Express `:id` (#737).
