---
"cartograph": minor
---

Recovers the data model of TypeORM codebases. An `@Entity` class imported from `typeorm` becomes a data entity with its own columns (type, primary key, nullable, stored column name) and its base class, and each `@ManyToOne`/`@OneToMany`/`@OneToOne`/`@ManyToMany` relation becomes a relation to the target entity the import names. `data_model.md` now also lists every entity's columns for all languages (SQLAlchemy, JPA, TypeORM), each citing its entity. On Vendure `packages/core` the data model goes from empty to 89 entities and about 470 relation fields.
