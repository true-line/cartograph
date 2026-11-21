---
"cartograph": minor
---

Terraform resources are now told apart by the root module that declares them, so two environments (for example `envs/prod` and `envs/staging`) that declare the same resource keep both declarations instead of one replacing the other. Terraform at the repository root keeps its existing ids. A repository's `state_json` still applies to a single root module in a subdirectory; with several root modules it is not applied and a Gap explains why.
