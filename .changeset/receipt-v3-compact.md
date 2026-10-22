---
"cartograph": minor
---

TypeScript primary-source receipts are now minted in a compact v3 format that stores each cited range once instead of repeating the file identity, cutting receipt size about 4–5×. More facts in large repositories fit within the receipt store's limits. Existing v1 and v2 receipts keep working unchanged.
