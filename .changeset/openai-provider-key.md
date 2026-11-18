---
"cartograph": minor
---

Settings → Provider keys now lists OpenAI beside Anthropic. An OpenAI API key is saved in the system credential store (Keychain on macOS, Credential Manager on Windows) as its own entry, and `OPENAI_API_KEY` overrides it. **Check connection** on the OpenAI row sends one bodiless request for OpenAI's model list with the OpenAI key only, and adds its size to the egress record under `openai:connection-check`. Saving a key sends nothing and opens no cloud step without consent.
