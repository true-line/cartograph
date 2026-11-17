---
"cartograph": patch
---

`security.md` no longer calls an endpoint unauthenticated when the tool only failed to recover a check. The finding is now titled **No authentication check recovered: {endpoint}**, the wording `endpoint-permissions.md` uses for the same fact, and the document states once that a check outside the source that was read, such as a gateway or middleware the adapters do not read, is not visible to the finding. The finding's type, identity, tier, confidence and evidence are unchanged.
