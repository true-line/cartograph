---
"cartograph": minor
---

Plugin adapters can declare their own coverage cells in their golden corpus. Each supported or partial cell names the golden cases that prove it, and Preflight and Settings show a plugin's cells once its conformance gate passes. A gate verdict now covers the plugin's corpus as well as its artifact, so editing the declared extensions or coverage after gating leaves the plugin ungated. The corpus also enters the recorded coverage basis, so repos ingested with a plugin show their coverage as stale until re-ingested after a corpus change. A corpus can name the language of an extension Cartograph does not know (`languages`). Run the gate once more for plugins gated before this change.
