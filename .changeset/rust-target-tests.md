---
"cartograph": minor
---

Rust `#[test]` and `#[tokio::test]` functions are now recovered as tests, linked to the functions they run and, for axum and actix-web test requests with literal paths, to the endpoints they call and the statuses they assert. SQL in test code no longer shows up as tables the system reads or writes.
