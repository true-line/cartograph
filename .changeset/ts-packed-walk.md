---
"cartograph": patch
---

The TypeScript directory walk now packs each file's facts once and keeps the incremental cache packed, cutting peak extraction memory on large repositories (#1571).
