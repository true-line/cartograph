---
"cartograph": minor
---

`security.md` now has an "Access control per capability" section: under each capability, every operation with who may call it as the recovered permission site states (a NestJS `@Allow` permission set, a Flask `login_required` guard, none recovered, or not established and why), each with a citation. On Vendure `packages/core` the document goes from "No explicit security findings" to access control for 310 operations under 42 capabilities; on SimpleLogin, 263 operations under 96, with the unguarded ones being sign-in, registration, OAuth and webhook routes.
