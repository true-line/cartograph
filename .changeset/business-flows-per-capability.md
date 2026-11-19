---
"cartograph": minor
---

D1 §1.6 Business flows now says what each flow does. With derived Capabilities, each operation gets one line under its capability: the handler, the functions it reaches in trace order, and the entities those functions write and read with their operations, citing the handler and data edges (SimpleLogin: `DELETE /api/user` is handled by `delete_user`, calls the audit-log helpers and writes `Job` (insert) and `UserAuditLog` (insert)). Operations handled with nothing further are listed together, and flows outside every capability keep the trigger table.
