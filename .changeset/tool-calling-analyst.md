---
"cartograph": minor
---

Adds `domain-analyst@29`, a domain analyst that works by calling tools natively on a local model, beside the current @28. Each run gets its own tool-calling runtime and loopback model proxy. Every request still passes the egress firewall and the step's own limits. A start on a cloud provider, or on a machine without the runtime, is refused before anything runs. Until the runtime ships inside the app, `CARTOGRAPH_AGENT_RUNTIME` names its entry script. Every other analyst version still uses the bounded loop.
