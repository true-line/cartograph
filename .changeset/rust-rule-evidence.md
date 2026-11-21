---
"cartograph": minor
---

Guarded `return`s in Rust functions are now recovered as rule evidence, like Python, TypeScript, Go and Java. The specification states them in words: a returned `StatusCode` reads as a refusal with that status, and an `Err(…)` as the error it fails with, by its message or variant name.
