---
"cartograph": patch
---

Kotlin functions, properties and parameters named `dynamic` or `suspend` no longer make the parser drop the declaration, or under stacked class annotations the whole class with its endpoints. A block comment before a declaration, such as after an expression-bodied function, no longer drops that declaration either.
