---
"cartograph": patch
---

Make language-plugin provenance host-authoritative. A plugin fact may only be T0 Deterministic and Confirmed, its evidence must cite the file the host handed the plugin with byte ranges inside it, and the host computes the content hash. A fact that breaks a rule fails that file's extraction with a named error instead of being stored.
