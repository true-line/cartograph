---
"cartograph": minor
---

Python recovery now reads SQLAlchemy declarative models. Each model with a declared table becomes a data entity with its columns, including inherited ones, and its foreign keys and relationships link entities in the data model. Anything that can't be proven, such as a missing or dynamic table name, a table claimed by two models, or a reference to an unknown table, is reported as a gap rather than guessed. On SimpleLogin, all 80 models are recovered.
