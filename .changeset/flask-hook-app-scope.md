---
"cartograph": minor
---

A Flask request hook that may block no longer makes the endpoints of a separate Flask application `unknown`. It applies only where the hook and the endpoint may share an application, and anything that can't be proven apart still keeps the hook.
