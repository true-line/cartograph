---
"cartograph": minor
---

Releases now also include a native Windows on ARM installer, `Cartograph_<version>_arm64_signed-setup.exe`, signed and timestamped through Azure Artifact Signing like the x64 installer. It is cross-built on x64 and installed and checked on a Windows ARM64 runner before it is attached (#227, ADR-0093).
