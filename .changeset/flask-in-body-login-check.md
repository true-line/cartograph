---
"cartograph": minor
---

A Flask-Admin view that no `is_accessible` guards is now recovered as requiring login when its own body turns anonymous users away before doing anything else: an `if not current_user.is_authenticated …` that returns a redirect or aborts with 401 or 403, written in the view or in a helper the view returns first. Where the body checks login in a shape that is not proven, the endpoint stays `unknown` and names the function holding the check, so a reviewer knows where to look.
