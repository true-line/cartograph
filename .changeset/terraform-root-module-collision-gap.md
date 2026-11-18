---
"cartograph": patch
---

When two Terraform root modules in one repository (for example `envs/prod` and `envs/staging`) declare the same resource address, the graph now records a Gap that names the address and every declaring file and span. Before, one of the declarations was dropped without any notice because both share one resource id. Resource ids are unchanged, and repositories without such a collision get the same graph as before.
