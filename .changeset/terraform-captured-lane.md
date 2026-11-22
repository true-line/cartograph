---
"cartograph": minor
---

Terraform files now join a source's capture when infrastructure is recovered. They are parsed from the retained bytes instead of the checkout, and each Terraform fact gets a source receipt you can open in the captured-source panel (#385, AC-1053).
