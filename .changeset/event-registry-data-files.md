---
"cartograph": minor
---

Event SDK call shapes and the deployed resources that back channels are now versioned data files instead of code. Cartograph also reads extra registry files from its `registries` folder in the app data directory. Each file is checked whole: an invalid one is rejected with its reason in the ingest summary and changes nothing, and every channel fact names the registry file that recognized it.
