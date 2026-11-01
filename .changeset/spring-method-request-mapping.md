---
"cartograph": patch
---

Java Spring controllers that declare handlers with method-level `@RequestMapping` (classic Spring MVC) now produce endpoints, one per proven verb, or an honest route gap. Before, they were silently dropped: a 61-repo OpenMRS estate recovered 2 endpoints from 592 such handlers.
