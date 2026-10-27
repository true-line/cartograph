---
"cartograph": minor
---

Flask-Admin views now appear as Flask endpoints: each `@expose` method of a registered view gets its composed path, its `<endpoint>.<method>` name and an access-check note, and library-generated admin routes are recorded as Gaps.
