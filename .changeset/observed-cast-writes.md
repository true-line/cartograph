---
"cartograph": patch
---

Observed infrastructure: a write through a cast, `!` or parentheses (`Object.assign(A as any, …)`, `(A!).X = …`) now voids the namespace it overwrites.
