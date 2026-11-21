---
"cartograph": minor
---

Kotlin: JUnit and `kotlin.test` test functions are now recovered as test cases, and the requests they make through Spring's MockMvc (the Kotlin DSL or `MockMvcRequestBuilders`) or Ktor's test client are linked to the Spring and Ktor endpoints they exercise, with the statuses they expect. Requests with computed URLs or paths no endpoint serves are reported as Gaps.
