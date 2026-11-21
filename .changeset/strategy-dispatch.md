---
"cartograph": minor
---

Calls through an injected strategy (`taxZoneStrategy.determineTaxZone(...)`) now reach the repository's classes that implement the strategy interface, so the business rules in default implementations such as `DefaultTaxZoneStrategy` are stated under the capabilities that call them (#2069).
