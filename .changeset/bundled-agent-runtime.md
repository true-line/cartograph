---
"cartograph": patch
---

The macOS app now ships its tool-calling runtime. `domain-analyst@29` runs on the bundled, signed `cartograph-agent` executable, so nothing has to be installed by hand. It is built from a committed lockfile with Pi pinned, on a pinned and checksum-verified Node release. The Claude lane is not in this build yet. `CARTOGRAPH_AGENT_RUNTIME` still overrides the bundled runtime for development.
