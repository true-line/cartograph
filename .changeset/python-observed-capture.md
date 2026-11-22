---
"cartograph": patch
---

Observed infrastructure in Python code (clients, HTTP calls, configuration reads) is now read from the source capture, the same bytes the Python parser sees, rather than the current checkout (#385, AC-1055).
