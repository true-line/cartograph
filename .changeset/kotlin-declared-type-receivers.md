---
"cartograph": minor
---

Kotlin calls on a parameter, property or local whose type is written in source (for example an injected `private val api: ApiService`) now resolve to that type's method, so call chains reach Retrofit interfaces and view models. Receivers with no stated type still resolve nothing.
