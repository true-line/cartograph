---
"cartograph": patch
---

Python absolute imports and Kotlin imports of extension functions, top-level properties, type aliases, annotation classes and enum entries now resolve to the repository file that declares them, instead of leaving an unresolved module Gap. On SimpleLogin, unresolved module edges drop from 177 to 17 in Python and from 69 to 4 in Kotlin.
