---
"cartograph": patch
---

Guarded effects no longer count as use-case guard candidates; builtins, constructor calls, directly imported logging verbs and module names shadowed by a local are not effects, and a module imported inside a function is. Rule statements keep nested `and`/`or` grouping and read only the request's own inputs as "given".
