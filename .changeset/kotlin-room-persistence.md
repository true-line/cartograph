---
"cartograph": minor
---

Kotlin: Android Room persistence is now recovered. `@Entity` classes become SQL tables with their columns, and `@Dao` functions read and write those tables, through literal `@Query` SQL or through the entity that an `@Insert`/`@Update`/`@Delete`/`@Upsert` parameter names. Computed queries, and tables or types that no entity declares, are reported as Gaps.
