---
"cartograph": minor
---

Python recovery now finds routes declared on Flask Blueprints and FastAPI `APIRouter`s, and gives each route its full path. A router's own prefix is joined with the prefix it is mounted under, even when the mount is in another file. Nested routers compose their whole mount chain. A route is confirmed only when its chain ends at an app the code is seen creating. A mount on an app passed in as a parameter, or a router whose mount was never observed, is marked as inferred rather than confirmed. A prefix that can't be read, or a cyclic chain, is reported as a gap.
