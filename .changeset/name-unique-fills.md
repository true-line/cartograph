---
"cartograph": minor
---

Unresolved TypeScript and JavaScript calls whose callee name matches exactly one function or method in scope now gain an inferred link to it, from a named deterministic rule rather than embeddings. The original gap stays visible and counted, the link is marked inferred rather than confirmed, and it is recomputed on every load, so a second symbol with the same name withdraws it. Semantic matching no longer spends effort on hops these links already cover.
