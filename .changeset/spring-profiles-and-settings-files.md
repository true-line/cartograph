---
"cartograph": minor
---

Spring Boot profiles are now environments in the environments document: a configuration document activated for a profile, and a profile-specific `application-<profile>` file whose profile a base file declares, each listed with the key paths it sets. No value is read. Undeclared profile files and settings files named like an environment (`settings/production.py`, `config/staging.yml`) are listed as not established.
