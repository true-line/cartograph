---
"cartograph": patch
---

EventBridge channel gaps can now be filled from OpenTelemetry traces. A span fills the gap only when it comes from the same source file and proves both the bus and the detail type (`aws.eventbridge.detail_type`) as one identity. Otherwise the gap stays, with a reason such as "detail type unobserved", "source attribution missing" or "2 distinct identities". Observed buses are kept as their own channel facts.
