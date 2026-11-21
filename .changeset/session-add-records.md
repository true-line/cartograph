---
"cartograph": minor
---

D1 §1.5 Changes: a record added with `Session.add(Model(…))` and a query delete (`Model.filter_by(…).delete()`) are now rule rows, e.g. SimpleLogin's "When no deleted alias with this email exists, a new “deleted alias” record is created."
