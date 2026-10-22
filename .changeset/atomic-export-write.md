---
"cartograph": patch
---

Saving an export over an existing file is now atomic: the content is written to a temporary file beside it and swapped in only when complete, so a full disk or failed write leaves the previous file intact.
