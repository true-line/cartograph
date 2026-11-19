---
"cartograph": minor
---

Derived capabilities now read as what a reader does with them: "Manage draft orders (admin API)" instead of "admin GraphQL operations in DraftOrderResolver", "Manage custom domain (dashboard)" instead of "HTTP operations in blueprint dashboard (app/dashboard/views/custom_domain.py)", "Reset password (authentication)" for a password-reset view. The verb comes from whether any operation changes state, the subject from the class or handler-file name without its framework words.
