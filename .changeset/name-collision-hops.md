---
"cartograph": minor
---

Semantic (T2) previews no longer propose a call whose name matches two or more symbols of the same name, such as `join` or `isEqual` helpers copied across modules. Similarity cannot tell such homonyms apart, so the hop stays an open gap tagged `name collision - needs import or type resolution` (#259, AC-1513).
