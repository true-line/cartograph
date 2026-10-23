---
"cartograph": minor
---

Observed infrastructure from Python now follows a client identity into another repository module's constant (`create_engine(config.DB_URI)` becomes the `DB_URI` env variable), citing the assignment it rests on and failing closed on anything that may rebind it (#734).
