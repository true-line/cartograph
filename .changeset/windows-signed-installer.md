---
"cartograph": minor
---

Releases now include a Windows installer, `Cartograph_<version>_x64_signed-setup.exe`. The installer, the app and both sidecars are Authenticode signed through Azure Artifact Signing and timestamped, so SmartScreen and enterprise policy can attribute them to a verified publisher. An unsigned Windows build is never published (#227, ADR-0093).
