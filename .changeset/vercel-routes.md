---
"cartograph": minor
---

Vercel Go functions are now routes: a Go file under `api/` exporting an `http.HandlerFunc` is an endpoint at its served path, and `vercel.json` rewrites onto it are endpoints at their public paths (`/docs/{path...}` → `/api/docs`). Unreadable rewrites are Gaps. On true-line/agent-bot this adds the sign-in, webhook, docs and hooks routes and takes D1 from 17 to 31 capabilities (#2010).
