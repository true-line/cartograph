---
"cartograph": minor
---

Flask routes that carry only flask-limiter rate limits or flask-cors `cross_origin` decorators now report no authentication instead of unknown, when those decorators are proven to come from those libraries. The result lists the decorators and records the assumption.
