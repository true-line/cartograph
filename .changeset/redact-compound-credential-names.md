---
"cartograph": patch
---

Redact credentials assigned to compound names (`DB_PASSWORD`, `clientSecret`, `AWS_SECRET_ACCESS_KEY`, `SECRET_KEY`) and to quoted keys (`"password": "…"`) before text is sent to a model, embedded or recorded. Previously only a bare `password`, `secret`, `api_key`, `access_token` or `auth_token` name was recognised. The redaction policy is now `redact-text@2`; cached embeddings and stored source-size measurements made under the previous policy are recomputed.
