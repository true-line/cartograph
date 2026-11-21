---
"cartograph": patch
---

Large repositories use much less memory during ingest. Source binding and ingest metrics now decode one fact at a time, and the knowledge phase reads its base without business-rule text it never uses. On a 5,360-file TypeScript repository, the ingest peak fell from 4.3 GB to 2.1 GB and the knowledge-phase peak from 3.8 GB to 2.8 GB.
