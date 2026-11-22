---
"cartograph": patch
---

The spec export uses less memory on repositories with many recovered business rules. Each rule's payload is held as compact text while the export compiles. On a 5,360-file TypeScript repository the export peak fell by about 0.5 GB, and the exported bundle is unchanged.
