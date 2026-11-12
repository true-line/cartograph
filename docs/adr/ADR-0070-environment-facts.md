# ADR-0070 — Environments as declared deployable sets, never as names

- **Status:** Proposed; owner review
- **Date:** 2026-11-12
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1791; #1844; #1745; US-0019 / AC-0661; `docs/DOCUMENT-CATALOGUE.md` D7 §7.2; [SPEC-14](../SPEC-14_environments.md); R-INT-1, R-INT-2, R-INT-5

## Context

D7 §7.2, "Environments and deployed resources", is a projected section. The
graph has no environment: no node label, and no property on a `Resource`
that says where it is deployed. Compose files are read by a line scan into
one `Tool` fact (service names and images). Kubernetes manifests are read
only for `CronJob` schedules. Kustomize and Helm are not read. An
environment file such as `staging.env` is a `Tool` fact holding key names;
its name is the only thing that says "staging".

A file name is a convention, not a declaration. A document that prints
"production" because a file is called `prod.yaml` asserts something the
source does not state.

## Decision

1. **An environment is a declared deployable set.** A file that declares a
   set of things to run together establishes one: a Compose file, and (next
   increment) a kustomize overlay. The fact is an `Environment` node whose
   identity is the declaring file's path. It is T0 Confirmed: the file
   exists and declares the set.
2. **Its name is what the file states.** A Compose top-level `name`, later a
   kustomize `namespace`. Where the file states none, the environment has no
   name and is shown by its path. The path is never turned into a name.
3. **A name that exists only in a file name is a Gap.** An environment file
   or settings file whose environment is carried only by its name yields a
   `Gap` with that reason and no `Environment`.
4. **What a set declares is a `Workload`,** a new label, linked by
   `DECLARES` from its `Environment`. It is not a `Resource`: nothing that
   reads `Resource` facts today (topology, permission findings,
   observed-state matching) changes. A Compose service is a `Workload` with
   its image, published ports, the names of its environment keys, its
   dependencies and its profiles, each read from the parsed file with spans.
5. **No value of an environment key is stored.** Names only. Image
   references and port mappings pass through redaction; a token-shaped
   scalar is withheld.
6. **No merge across files.** Which Compose files a deployment combines
   (`-f a -f b`) is not declared in them. Each file is its own environment;
   the document shows a service that several files declare side by side,
   with each declaration's span, and does not say one overrides another.
7. **Lowest tier.** A parse that fails, or a construct the reader does not
   evaluate (anchors, aliases, tags, `extends`, `include`, variable
   interpolation in a service key), is a Gap or a recorded limit. Nothing is
   guessed.

## Consequences

- A repository with a Compose file gains `Environment`, `Workload` and
  `DECLARES` facts, so its graph content hash changes once. The existing
  Compose `Tool` fact is unchanged.
- The same service in `docker-compose.yml` and `docker-compose.prod.yml`
  appears as two declarations in two unnamed environments. The reader of the
  document sees the file names and draws their own conclusion.
- Terraform resources are not attached to an environment by this decision.
  Root modules that declare the same address collapse today (#1844); that is
  decided first.
- Kubernetes manifests, kustomize overlays, Helm values and per-environment
  settings files are stated as not covered until their increments land.

## Alternatives

1. **Name environments from file and directory names.** Matches common
   practice and reads well, but asserts what no file states (requirement 3 of
   #1791).
2. **Reuse `Resource` for Compose services.** No new label, but it changes
   `topology.md`, observed-state matching and every frozen output that lists
   resources, in the same change that introduces the reader.
3. **Merge override files into one effective environment.** Requires knowing
   the command line a deployment uses, which the repository does not state.
