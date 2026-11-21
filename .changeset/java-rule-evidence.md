---
"cartograph": minor
---

Java methods now yield business rules (#2020): a `throw` or a valued `return` under `if`/`else` branches is a guarded exit with its sanitized conditions, owned by its method, like the Python, TypeScript and Go producers, and D1 states a thrown exception by its message or its kind ("it fails with the error “Invalid id”"). On the JHipster sample app this recovers 89 rules across 20 files, such as `userDTO.getId() != null` → `BadRequestAlertException("A new user cannot already have an ID")`.
