---
"cartograph": patch
---

Python route mounts now confirm a path only when the receiving application's assignment actually reaches the mount: it must be the name's only binding in its scope, unconditional and written before the mount. Rebound, conditionally assigned, later-assigned or closure-captured receivers stay InferredStrong (`mount_receiver_unproven`) instead of being reported as Confirmed (#747).

A function-local application assignment elsewhere in a file no longer hides that file's module-level application, so mounts through the module-level app confirm as they should.
