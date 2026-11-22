---
"cartograph": minor
---

Semantic admission can now run on a receipt from the locked evaluation runner. With `locked-receipt.json` and `runner-key.txt` in the app's evaluation store, the app holds no dataset or labels: it verifies the receipt against the pinned key and rechecks it against its own policy, embedding space and corpora before any family admits (ADR-0082).
