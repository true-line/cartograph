---
"cartograph": minor
---

Rust code that runs literal SQL through `rusqlite`, `postgres`, `tokio-postgres`, `sqlx` or `diesel` now shows the tables each function reads and writes, with their operations and columns. Statements built with `format!` around a computed table are reported as gaps.
