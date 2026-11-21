---
"cartograph": patch
---

The Spec Workbench export uses less memory on large repositories. Each artifact is cut down to its preview as soon as it is compiled, instead of after the whole bundle has been built, and what the Workbench shows is unchanged. On a 5,360-file TypeScript repository the export peak fell from 5.6 GB to 4.9 GB.
