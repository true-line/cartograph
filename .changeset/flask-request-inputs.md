---
"cartograph": minor
---

Flask endpoints now record the literal query, form and JSON body names their handler reads, marked partial when a read is not literal, and the API contract scorer measures them against a documented contract.
