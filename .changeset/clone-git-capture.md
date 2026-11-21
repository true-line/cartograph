---
"cartograph": minor
---

Cloned repositories are now parsed from the exact Git objects of the cloned commit instead of the checked-out files, so evidence citations match the commit byte for byte. A selected file missing from that commit stops the recovery instead of falling back to the checkout.
