---
"cartograph": minor
---

Recover a data model from Java JPA code: import-proven `@Entity` classes become cited data entities with their table, fields and `@Id`, `@OneToMany`/`@ManyToOne`/`@OneToOne`/`@ManyToMany` fields become relations with cardinality and `@JoinColumn`, and Spring Data repositories bind to their entity so repository calls (`save`, `findById`, `deleteById`, …) show as reads and writes. Computed table names and relations to non-entities stay explicit gaps. The recovered data model document now lists entity relations alongside access edges.
