---
"cartograph": patch
---

Reclaiming superseded receipts after a recovery now reads only that source's receipts instead of rebuilding and decoding every receipt of every source in the store, so recovering a small repository no longer pays for the others. A damaged receipt of another source no longer blocks this cleanup. It is still refused when it is read or when its own source is cleaned up. Stored receipts and their ids are unchanged.
