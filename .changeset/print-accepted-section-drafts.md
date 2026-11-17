---
"cartograph": minor
---

`product-specification.md` now prints an accepted section draft in its section. Each claim is shown with its citation ids, marked T3 / InferredWeak, with the role that wrote it and "human-reviewed: accepted". A draft is printed only while every fact it cites is unchanged; one that is stale, replaced, in conflict or citing a missing fact is named with its reason instead. A `verified-only` export prints no drafted claim and says how many it left out of each section. The document is now readable over MCP with `cartograph_read_artifact`, as the same bytes the export writes. No agent drafts a section yet, so existing exports show "Not drafted" as before.
