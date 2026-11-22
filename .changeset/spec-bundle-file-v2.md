---
"cartograph": minor
---

Export bundle writes the smaller v2 bundle file (ADR-0086): each artifact's content appears once, under `files`; evidence repos and commits, and assertions two artifacts share, are listed once and referenced by index; and the JSON is compact with one entry per line. Scripts that read `artifacts[].content` or an evidence row's `repo` directly need the v2 shape, or the spec crate's `read_bundle_file`, which reads v1 and v2 files alike.
