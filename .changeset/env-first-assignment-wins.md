---
"cartograph": patch
---

An allowlisted env key whose first assignment is dropped (multiline or secret-shaped) no longer takes a later duplicate's value in toolchain facts: the first assignment wins, as documented (#933).
