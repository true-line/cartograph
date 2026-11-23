---
"cartograph": patch
---

A recovered flask-limiter rate limit now records the 429 status a request past it is refused with, so a business rule stating the limit and its refusal can cite the endpoint.
