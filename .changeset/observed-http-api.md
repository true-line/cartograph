---
"cartograph": minor
---

Observed infrastructure now includes HTTP APIs: calls through the platform `fetch`, axios or ky whose URL starts with a proven absolute origin use one `http-api` resource per origin, whichever client made them. Relative URLs name no resource, a host the code leaves open is never truncated to a guessed origin, an env-led base records its variable name, and a computed axios or ky URL is a Gap.
