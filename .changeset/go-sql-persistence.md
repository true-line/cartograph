---
"cartograph": patch
---

Go repositories now have their SQL persistence recovered (#2019): `database/sql` and `sqlx` calls whose statement is a literal, a constant or a local the function builds (through `+`, `+=` and `fmt.Sprintf`) read and write the tables they name, each table a data entity with the columns its inserts and updates set. A statement that names no table and is computed is a gap. On miniflux this recovers 15 tables, 91 reads and 65 writes, with 3 gaps where a query arrives as a parameter.
