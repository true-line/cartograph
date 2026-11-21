---
"cartograph": minor
---

Recover more business rules (#1998). D1 §1.2 now has a "What each guard checks" table that lists the refusals a repository guard and its wrapper reach. D4 and D1 tie response keys to their statuses. Rule rows state a configuration constant's default and its environment variable, read from a new numeric or boolean `defaults` field on Python `ConfigRead` nodes. Deletes and updates made through a model classmethod that returns a query (`Alias.filter(...).delete()`) are now recovered as writes.
