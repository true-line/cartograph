---
"cartograph": patch
---

The product specification (`product-specification.md`) now fills 1.2 Actors and roles and 1.3 Capabilities from the knowledge layer instead of saying "No `Capability` fact is in this export". 1.3 lists each derived Capability with its operation count and examples; 1.2 groups the derived caller sets into roles by the permissions their guards list (on Vendure `packages/core`: 93 permission sets over 289 guarded operations). Both are marked Semantic / InferredWeak, and a `verified-only` export still withholds them.
