---
"cartograph": patch
---

TypeScript ingest no longer crashes on a comment inside an arrow function's parameter list, such as `(x, /* note */) => x` (found on VSCode).
