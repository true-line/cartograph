---
"cartograph": patch
---

Export bundle no longer holds the whole bundle in memory before writing it. Each artifact is written to a temporary file as soon as it is compiled, so very large repositories can be exported. On a 16,180-file TypeScript repository the export's memory peak fell from 13.4 GB, which ran out of memory, to about 10 GB. The exported file is unchanged.
