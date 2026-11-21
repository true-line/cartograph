---
"cartograph": minor
---

Kotlin: JetBrains Exposed tables are now recovered. `object`s extending an Exposed `Table` or id table become SQL tables with their columns, and DSL statements called on them (`selectAll`, `insert`, `update`, `upsert`, `deleteWhere` and the like) read and write those tables. Computed table names, and statements on receivers that are not recovered tables, are reported as Gaps. Room entity properties whose accessor only reads another value's `field` member are no longer taken for columns.
