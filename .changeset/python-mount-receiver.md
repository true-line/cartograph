---
"cartograph": patch
---

FastAPI and Flask mounts on a function-local app now confirm a route only when the app's sole binding in its scope is the framework constructor, ahead of the call. A rebound, late-constructed, branch- or loop-assigned, `nonlocal`- or `global`-rebound receiver leaves the route unproven instead of fabricating a Confirmed path.
