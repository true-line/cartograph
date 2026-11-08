---
"cartograph": patch
---

Hosted Opus investigations reserve 16,384 output tokens per call, because Opus 5.5's always-on thinking counts toward the limit. A provider stop at the output limit now reports `output_limit_reached` instead of `incomplete_response`.
