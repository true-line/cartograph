---
"cartograph": minor
---

New analyst version `domain-analyst@32`: when a required source could not be read for lack of prompt capacity, the app now records that limitation itself instead of asking the model to, so an answer is no longer refused and lost over how that sentence was worded (#1728).
