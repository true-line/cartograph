---
"cartograph": minor
---

Kotlin: Ktor server routes are now recovered. Import-proven `routing { }` blocks and `Route` extension functions yield Endpoints with `route("…")` prefixes composed, including groups mounted from another file, and each handler lambda becomes a Symbol that owns its calls. Computed paths and unmounted or ambiguous groups are reported as Gaps.
