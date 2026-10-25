---
"cartograph": minor
---

NestJS GraphQL resolvers are recovered: each `@Query()` and `@Mutation()` method becomes a GraphQL endpoint with its field, the API it is assumed to be mounted on, and any `@Allow` permissions, so its flows can be traced like HTTP routes.
