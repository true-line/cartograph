---
"cartograph": minor
---

Record the full `HEAD` commit SHA when recovering a clean local git checkout instead of `"workdir"`, fail closed before publishing if a clean checkout changes mid-recovery, and attach a bounded `commit_note` to `RepoCoverage` explaining why non-clean or non-repository local trees remain `"workdir"`.
