---
"cartograph": patch
---

Documentation references link to code again. After a recent change, a README or doc that named a file, a route or a resource was stored without its link to that fact, and documentation drift was not compared, because the reference index saw only repository nodes. The step that links found ADRs and reads documentation after a recovery also no longer loads the whole graph into memory: on the REA benchmark the peak memory of a recovery drops from 6.73 GB to 4.89 GB.
