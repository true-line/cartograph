---
"cartograph": patch
---

Recovering a large TypeScript or JavaScript source now needs far less memory while it reads the code. The recovered facts are kept in their compact stored form file by file instead of as one in-memory tree for the whole repository. On the REA benchmark (5,360 files, 26,278 rules, 161,569 gaps) the peak of this step drops from 4.58 GB to 1.31 GB. The recovered graph and its citations are unchanged.
