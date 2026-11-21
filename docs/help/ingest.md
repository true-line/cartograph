# Ingest: Connect → Preflight → Recover

Recovery is a three-step, local-only flow:

1. **Connect** — point at a GitHub repo, a local folder, or a system manifest
   (multiple repos as one system). Nothing leaves the device.
2. **Preflight** — a pure filesystem scan detects languages, frameworks, and
   adapter coverage, and classifies risky constructs before any recovery:
   potential gaps vs unsupported patterns, never conflated. For a system
   manifest, each local repository it declares is scanned relative to the
   manifest's folder and listed with its own results; remote and managed
   entries are shown as deferred and are not fetched until recovery. The
   manifest preview records nothing: recovery records the findings.
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

## Env-file values

Cartograph reads key names from `.env` and `.env.*` files, never their
values, unless you list a key. To let it resolve a queue name or a base URL
from an env file, list the key exactly (no globs):

- in `cartograph.system.toml`: `env_read = ["ORDERS_QUEUE", "API_BASE"]`;
- in any repository, in `.cartograph/env.toml`: `read = ["ORDERS_QUEUE"]`.

A key a site uses that an env file sets but no list names is reported as a
gap that names the key and the file. A URL value never keeps a `user@`
part. An env file that holds a secret still stops the ingest, whether the
key is listed or not. A value declared under `[env]` in
`cartograph.system.toml` needs no listing.

A listed value also appears in the env file's toolchain facts, citing its
line. Opening that evidence shows the line alone, never the lines around
it. A value from a template (`.env.example`, `env.sample`, `.env.dist`) is
marked as a template, and a value a real env file sets always wins over a
template's.

## Coverage inventory

After a recovery, the Workspace lists each repo's coverage inventory before
it offers any investigation. The inventory says which deterministic producers
(language adapters, Terraform, toolchain configuration, gated plugins) and
observational producers (Terraform state, Pulumi deployments, OpenTelemetry
traces) ran. Each producer has a stopping reason scoped to that repo,
revision and layer hints. The inventory also lists:

- producers the manifest's layer hints excluded, including a declared state
  or Pulumi file that was therefore never read;
- inputs that were not declared but would have enriched facts here, with the
  `cartograph.system.toml` field to add;
- capabilities no installed producer covers. These are tool limitations, not
  system gaps;
- files a gated plugin was refused on. When a plugin emits a fact the host
  does not accept, returns an error or traps, or exceeds its fuel, memory or
  table bound on one file, that file is skipped for that plugin and the
  recovery continues. The plugin's row gives the refused count by reason,
  lists the files as not covered, and leaves them out of its file count. No
  fact from a refused file by that plugin is in the graph. At most 256 files
  are listed per plugin; the rest are counted.

"Complete within the declared scope only" never means global completeness.
Anything excluded, missing, unsupported or refused marks the repo as limited.

A plugin that runs past its time limit on a file still fails the whole
recovery. How long a file takes depends on the machine, so skipping it would
give different graphs for the same commit.

## Create a system manifest

In **Connect a target**, choose **System manifest**, then **Create new manifest**.
Add the repositories that form your system. Relative paths resolve from the
folder where you save the manifest. Add configuration keys exactly as the code
reads them, including case and punctuation (for example `SETTINGS.apiUrl`).
Declare each service's repository and base URLs explicitly; add gateway routes
only when you know their prefix and target service.

**Preview manifest** validates those declarations and shows the TOML. Editing
any field invalidates the preview. **Save new manifest** opens the native save
panel and selects the saved path only after the write succeeds. Choose a new
file: existing manifests, including their advanced settings, are never replaced.
Cancelling the save or encountering an error keeps your draft. Close the guide
and run **Preflight** when ready; drafting and saving do not scan repositories,
contact remote services, or start recovery. Declarations help link client calls,
but ambiguous or unresolved calls remain gaps.
