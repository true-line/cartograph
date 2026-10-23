---
"cartograph": patch
---

Publishing an extraction into the graph no longer holds a second whole-graph copy: the snapshot check streams the stored graph and compares it row by row, and the patch is built from borrowed snapshot facts with incident edges indexed once. On the SimpleLogin showcase, the publish heap peak drops from 735 / 509 / 576 MB to 535 / 303 / 357 MB per repo, and process RSS from about 1.9 GB to 1.41 GB (#725).
