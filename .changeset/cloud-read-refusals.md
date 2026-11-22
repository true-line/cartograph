---
"cartograph": minor
---

On a cloud provider, a captured source read whose path matches a read-deny rule (environment files, key material, credential files) is now withheld from the model instead of being sent. The read and its firewall refusal are recorded together, and the run continues (#679).
