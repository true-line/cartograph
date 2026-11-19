---
"cartograph": minor
---

Python callers are now named by the guard that admits them. The product specification's §1.2 adds a "Callers by recovered guard" table (signed-in user via `login_required`, a caller passing an API guard such as `require_api_auth`, Flask-Admin views, and any caller where no guard is recovered); user stories say "As a signed-in user, I can view coupon" instead of "As an API caller, I can coupon"; and `security.md` and `endpoint-permissions` print guards by name instead of symbol id.
