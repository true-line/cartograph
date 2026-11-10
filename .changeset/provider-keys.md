---
"cartograph": minor
---

Settings has a new Provider keys section. Enter, replace or remove a provider API key in the app; it is stored in the system credential store (Keychain on macOS, Credential Manager on Windows) and never shown again. The `ANTHROPIC_API_KEY` environment variable still works and takes precedence. A release build opened from Finder can now use the cloud lane. Consent is unchanged: each cloud step still asks.
