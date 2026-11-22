---
"cartograph": patch
---

The spec export uses less memory when it builds the fact index for a large repository. On a 5,360-file TypeScript repository the export peak fell by about 190 MB, and the exported bundle is unchanged.
