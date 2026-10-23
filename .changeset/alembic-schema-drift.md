---
"cartograph": minor
---

Python repositories with an `alembic.ini` now replay their Alembic migration history and report schema drift against the SQLAlchemy models, both ways, in the drift register's new Schema drift section; anything the replay cannot follow is a Gap (#833).
