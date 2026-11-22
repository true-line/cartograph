---
"cartograph": patch
---

Spec exports of very large repositories hold the fact and evidence index in less memory: the names, paths and commits that repeat across its rows are kept once. On a 16,180-file TypeScript repository the export's peak fell by about 0.5 GB, and every exported file is unchanged.
