---
"cartograph": minor
---

Python code that writes SQLAlchemy models, directly through a proven session or through a model helper classmethod such as `Alias.create(…)`, now records `WRITES` edges to the model's data entity. Journeys whose goal endpoint is fetched from a screen now count those writes, so they can reach an observed-write goal.
