---
"cartograph": minor
---

SQLAlchemy `ForeignKey(Model.column)` targets now resolve to the recovered model when the declaring file names it and the column is declared, instead of always becoming a Gap.
