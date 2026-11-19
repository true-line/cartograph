---
"cartograph": minor
---

`journeys.md` now reads as a path through the application instead of a list of graph ids. Each journey opens with a **Path:** line, for example "`/` (index) → `/dashboard/` (via `set_index_page.index`, conditional) → `/dashboard/mailbox` (via a link in `templates/menu.html`) → …, then POST /dashboard/mailbox/<int:mailbox_id>/, which writes `AuthorizedAddress`, `UserAuditLog`", and the steps, goal, writes, flows and tests print page routes, handler names, operations and table names. Ids remain on the `Id` line, in the fact index and in `bundle.json`.
