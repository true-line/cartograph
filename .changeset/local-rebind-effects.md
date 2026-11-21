---
"cartograph": minor
---

D1 states when a local is discarded or cleaned under a branch: `alias = None` as "the alias is discarded" (an alias that belongs to another user is never returned) and `new_name = new_name.replace("\n", "")` as "line breaks are removed from the new name" (#1998).
