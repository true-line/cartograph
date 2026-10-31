---
"cartograph": patch
---

Recovered user stories render capability names, ids and inline provenance as literal Markdown text, so stored HTML, fences, backticks and headings stay inert (#1453). The artifact viewer decodes these escapes, so it still shows the exact text.
