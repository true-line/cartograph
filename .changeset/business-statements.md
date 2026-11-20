---
"cartograph": minor
---

The product specification now states rules, callers and capabilities in business terms (#1999). Each rule row has a Rule sentence in domain terms, such as "When the user cannot create new alias, the request is refused with status 400", with the source condition kept beside it as evidence, or a Gap when the condition is outside the rendering rules. Callers are named by role: API client, administrator, Twilio webhook. Capability names no longer carry file paths.
