---
"cartograph": minor
---

Kubernetes `CronJob` manifests now become `Schedule` flow triggers, one per container, entering the script its command runs; entries mapped through the root `Dockerfile` are marked as an image assumption (#779).
