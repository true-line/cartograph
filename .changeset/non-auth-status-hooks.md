---
"cartograph": minor
---

Flask auth: a request hook that only ends requests with a literal 429 or 5xx status (except 511) (such as a maintenance-mode hook) no longer makes routes unknown.
