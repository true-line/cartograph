---
"cartograph": patch
---

Directory recovery now holds TypeScript receipts in a compact form: each file's identity is stored once and shared by its receipts, and the full receipt is rebuilt when needed. The receipt budgets measure this retained memory, so a large repository keeps more of its receipts citable within the same limits. Persistence also writes receipts one at a time instead of holding the whole batch's JSON (#725).
