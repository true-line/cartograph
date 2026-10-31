---
"cartograph": patch
---

Selected-compiler logical accounting now counts security findings with a proven per-row JSON value count, instead of one value per serialized output byte. This lowers the reservation floor the MCP read service derives for official artifacts. At 4 MiB / 5,000 rows / 100k values it falls from about 19 GB to about 9.5 GB.
