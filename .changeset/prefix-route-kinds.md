---
"cartograph": patch
---

Prefix routes now declare how they match (`prefix_kind`: node-http raw `startsWith`, Next.js one-or-more or zero-or-more catch-alls). Same-repo and cross-repo fetch stitching match by that kind and keep overlapping matches as ambiguity Gaps. Cross-repo joins also reach method-less `ALL`/`ANY` routes.
