---
"cartograph": patch
---

Findings accepted as curated now appear in the curated reads (ADR-0074). A best-effort export prints each eligible finding in `curated.md` as a cited statement marked InferredWeak, with its role, limitations and source support, and an agent reads the same through `curated` and `curated-json`. A verified-only export prints none and says how many it left out. Findings whose cited facts changed or are gone are listed with the reason, without their text. The app does not yet offer the review or draw the list.
