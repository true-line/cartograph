---
"cartograph": minor
---

Kotlin: JPA entities are now recovered. Classes annotated with a Jakarta or javax `@Entity` become JPA data entities with their table and fields (`@Id`, `@Column`, `@Transient`), in the same shape as Java's, and relation properties (`@ManyToOne`, `@OneToMany` and the rest) link them to their target entities. Computed names and relation targets that are not recovered entities are reported as Gaps.
