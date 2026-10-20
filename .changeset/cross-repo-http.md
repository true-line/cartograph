---
"cartograph": minor
---

Client calls now cross repos when the system manifest says how. A system manifest can declare services, with the repo that serves each and its base URLs, and gateway routes that forward a path prefix to a service. After every repo is ingested, an absolute-URL call that one of these covers is joined to the matching endpoint in the serving repo, and the edge cites the declaration. A call no declaration covers, or one with several equally supported targets, stays a gap that says why.
