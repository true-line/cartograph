# Ingest: Connect → Preflight → Recover

Recovery is a three-step, local-only flow:

1. **Connect** — point at a GitHub repo, a local folder, or a system manifest
   (multiple repos as one system). Nothing leaves the device.
2. **Preflight** — a pure filesystem scan detects languages, frameworks, and
   adapter coverage, and classifies risky constructs before any recovery:
   potential gaps vs unsupported patterns, never conflated.
3. **Recover** — deterministic extraction runs as a durable, cancellable job;
   the summary reports per-language file/node/edge counts, including zeros.

## Dropping folders

On the Connect step you can also drop one or more folders onto the window.
Each one ingests through the same path as a typed target: a plain folder as
a local folder, and a folder holding `cartograph.system.toml` (or that file
itself) as a system manifest. Folders ingest one at a time, in drop order.

Every dropped path gets its own outcome:

- **Rejected** before any work, with the reason: not found, not a folder, a
  repeat, a folder inside another dropped folder, or more than 64 paths.
- **Ingested**, listed with each repo and its commit identity. A local folder's
  commit is `workdir`.
- **Failed**, with the error the ingest reported.

The set is only reported as fully ingested when every path ingested. The list
stays on Connect until you dismiss it, and it cannot be dismissed while a
folder is still queued or ingesting. A later drop is listed below the
earlier outcomes. A drop that arrives while intake is running is rejected,
not lost.
