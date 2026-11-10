---
"cartograph": patch
---

The egress total now counts every cloud payload that was sent, including calls that then failed or timed out. Previously only calls that returned a usable answer were counted. A cloud call is not made if its egress cannot be recorded.
