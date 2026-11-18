---
"cartograph": patch
---

Preflight now previews a system manifest instead of skipping it. Each local repository the manifest declares is scanned relative to the manifest's folder and shows its detected languages, frameworks and adapter coverage. Remote and managed entries are listed as deferred and are not fetched. The preview registers no source and records no finding; recovery still records the findings. A preview that fails or is cancelled shows no partial results.
