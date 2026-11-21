---
"cartograph": minor
---

Kotlin: Spring Data repositories are now read. An interface extending `JpaRepository`, `CrudRepository` or another Spring Data base is linked to the JPA entity it manages, and calls such as `findById`, `save` or `deleteAll` on a repository-typed parameter, property or local become reads and writes of that entity from the calling function. A repository whose entity is not a recovered JPA entity is reported as a Gap.
