---
"cartograph": minor
---

Kotlin screens that get their view model from Hilt's `hiltViewModel` (by type argument, or through an assisted factory callback) now resolve calls on that view model, including unqualified calls inside `with(hiltViewModel<VM>()) { … }`.
