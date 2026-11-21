---
"cartograph": patch
---

The Java adapter records Bean Validation constraints (`@NotNull`, `@Size`, `@Pattern` and the other built-ins) on the Spring endpoints whose handlers validate them, with Gaps for custom constraints, validation groups and cascaded fields (#2162).
