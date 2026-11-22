---
"cartograph": minor
---

Raise the embedding plan, working set and vector cache bound to 1,048,576 rows (4 GiB of vectors, 512 MiB of plan text) so a whole-VSCode semantic preview (476,907 candidates plus 81,654 hops) embeds in one pass (#259, AC-1507).
