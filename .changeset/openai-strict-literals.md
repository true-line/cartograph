---
"cartograph": patch
---

Fix hosted OpenAI investigations, which failed on their first call with an HTTP error. The strict response schema now gives every literal an explicit type and spells out object and array constants, as the OpenAI API requires.
