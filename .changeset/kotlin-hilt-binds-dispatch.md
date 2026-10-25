---
"cartograph": minor
---

Kotlin calls on an interface bound with a Hilt `@Binds` in a `SingletonComponent` module now continue to the bound implementation, so Android journeys can reach the endpoint the implementation calls. The journey records that hop as a `hilt_dispatch` assumption.
