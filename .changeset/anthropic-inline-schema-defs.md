---
"cartograph": patch
---

The response schema sent to Anthropic for hosted Claude investigation calls is now in a form the Messages API accepts. References are inlined instead of carried as `$defs`, and exact structured choices are sent as closed object schemas instead of structured `const`.
