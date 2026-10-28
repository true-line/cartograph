---
"cartograph": minor
---

The Java adapter now recovers JAX-RS resource endpoints (`jakarta.ws.rs` and `javax.ws.rs`). Methods it cannot place, such as inherited resource methods, interface methods, sub-resource locators and dynamic paths, become explicit route Gaps.
