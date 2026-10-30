---
"cartograph": patch
---

Provenance can now record its evidence basis, separately from its tier: source, artifact or observation (ADR-0051). Artifact evidence cites the artifact's content digest, its kind and an offset or member path. Existing graphs and exports are unchanged, and read as `source` (or `observation` for T1 facts). No artifact reader ships yet (#1444).
