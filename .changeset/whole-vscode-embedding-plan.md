---
"cartograph": minor
---

Raise the embedding plan, working set and vector cache bound to 1,048,576 rows (3 GiB of vectors, 512 MiB of plan text) so a whole-VSCode semantic preview (476,907 candidates plus 81,654 hops) embeds in one pass. Sealing no longer copies the vector set and the ANN index frees each returned vector as it is added, so peak vector memory stays near twice the set (#259, AC-1507).
