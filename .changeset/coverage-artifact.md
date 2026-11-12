---
"cartograph": minor
---

Every export now includes `coverage.md` and `coverage.json`. They show, for each repository, which producers ran and how many files each read, which were excluded, which inputs were missing and which languages or constructs no producer covers, each with its recorded reason and the repository's inventory hash. Before, a reader of the exported documents could not tell an absent finding from a file that was never read. The two files are the same in `verified-only` and `best-effort` exports.
