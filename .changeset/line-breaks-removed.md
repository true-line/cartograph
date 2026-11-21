---
"cartograph": patch
---

D1 states `x.name = x.name.replace("\n", "")` as "line breaks are removed from the alias's name" and `"\n" in alias.name` as "the alias's name contains a line break", instead of a Gap (#1998).
