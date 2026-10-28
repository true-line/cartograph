---
"cartograph": minor
---

Go `net/http` client calls (`http.Get`/`Head`/`Post`/`PostForm`, `http.DefaultClient` verbs and `http.NewRequest`) now become fetch sites that stitch to same-repo endpoints or cited Gaps, also in client-only manifests (#1279). Method-less routes such as Go `HandleFunc("/x", …)` now match a fetch of any method.
