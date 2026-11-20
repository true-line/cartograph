---
"cartograph": patch
---

Follow a check that several functions share a name with to the one on the caller's receiver class (#2029): `g.user.is_active()` now shows what `User.is_active` checks, instead of "5 repository functions have this name".
