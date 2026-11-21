---
"cartograph": minor
---

Kotlin: what starts a Kotlin system is now recovered as entry points. `fun main` (top level, or `@JvmStatic` in an object) is a process start, and an HTTP server when it starts Ktor's `embeddedServer` or Spring Boot with web endpoints; Spring message listeners are queue consumers with their topics, `@Scheduled` functions are scheduled jobs, and `CommandLineRunner`/`ApplicationRunner` runners are startup tasks.
