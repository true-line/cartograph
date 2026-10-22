---
"cartograph": minor
---

Python recovery now finds routes declared on Flask Blueprints and FastAPI `APIRouter`s, and gives each route its full path. A router's own prefix is joined with the prefix it is mounted under, even when the mount is in another file. A prefix that can't be read is reported as a gap, and a router whose mount was never observed is marked as such rather than presented as confirmed.
