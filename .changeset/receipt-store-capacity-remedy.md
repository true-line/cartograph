---
"cartograph": patch
---

When the retained receipt store is full, the coverage reason for the receipts it could not keep now says how to free space: forget a retained source, or recover a source again so its superseded receipts are reclaimed. It used to say the store could not reclaim space, which stopped being true once forget and supersession reclaimed receipts nothing pins.
