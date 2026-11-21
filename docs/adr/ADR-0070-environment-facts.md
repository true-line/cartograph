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

8. **Kustomize (increment 2).** Every kustomization file is an
   `Environment`; a base that overlays include is one too, shown as included.
   It declares the pod-bearing manifest workloads its `resources` chain
   reaches inside the repository. A patch file is a `PATCHES` relation that
   states what the patch writes; nothing is merged, and transformers are
   named, not applied.

9. **Helm (increment 3).** A chart's `Chart.yaml` is a `Workload` of kind
   `helm-chart` (name, version, appVersion, type, dependency names) carrying
   the sorted key paths of its default `values.yaml`, never a value.
   Templates are not rendered, so the objects a chart creates are not
   listed. A helmfile release is an `Environment` with id
   `env:{repo}@{helmfile}#{release}`, named only by a namespace the release
   states, with `DECLARES` to the local chart it installs; the edge lists
   each values file the release names with that file's key paths. A
   templated helmfile, `bases`, `helmfiles`, a remote or missing chart,
   inline values, `set`, `setString`, `secrets` and `valuesTemplate` are
   Gaps naming the construct. A chart values file no release names whose
   file name names an environment (`values-prod.yaml`) is a name-only Gap
   (decision 3).

10. **Spring Boot profiles and settings files (increment 3).** A document of
    a base `application.{yml,yaml,properties}` that states
    `spring.config.activate.on-profile` (or document-level `spring.profiles`)
    is an `Environment` `env:{repo}@{file}#{profile}` named by that profile.
    A profile-specific file `application-{profile}.*` is an `Environment`
    only where a base file of its directory declares the profile
    (`spring.profiles.active`, `.include`, a `spring.profiles.group` entry,
    or an activated document); it is cited at that declaration, never in
    itself. Each lists the key paths it sets, never a value. A profile
    expression is a Gap. An undeclared profile file, and a settings module or
    configuration file named like an environment (`settings/production.py`,
    `config/staging.yml`, `config/environments/production.rb`), is a
    name-only Gap (decision 3).

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
- Helm charts and values, per-environment settings files, and manifest kinds
  other than `Deployment`, `StatefulSet`, `DaemonSet` and `Job` are stated as
  not covered until their increments land.

## Alternatives

1. **Name environments from file and directory names.** Matches common
   practice and reads well, but asserts what no file states (requirement 3 of
   #1791).
2. **Reuse `Resource` for Compose services.** No new label, but it changes
   `topology.md`, observed-state matching and every frozen output that lists
   resources, in the same change that introduces the reader.
3. **Merge override files into one effective environment.** Requires knowing
   the command line a deployment uses, which the repository does not state.

## Addendum — Terraform resource ids scoped by root module (#1844)

- **Status:** Proposed; owner decision
- **Date:** 2026-11-18
- **Related:** #1844; #1791; AC-0007, AC-0009, AC-0048, AC-0051..0052, AC-0225, AC-0316

### Context

A Terraform resource id is `res:{repo}@{address}` (`iac::resource_id`). The
directory walk reads every `.tf` file in the repository and gives none of
them a root-module scope, so `envs/prod/main.tf` and `envs/staging/main.tf`
that both declare `aws_sqs_queue.orders` produce one id. The store upserts
on id and keeps one declaration; the other is lost without a trace. This is
the collapse the Consequences above defer. Environments built on Terraform
cannot be described until each root module's resources are distinct facts.

The interim fix (#1844, first PR) keeps ids and emits a
`gap:tf-address-collision:{repo}@{address}` Gap that cites every declaring
span. It makes the loss visible; it does not stop it.

### Decision (proposed)

1. **Ids carry the root module.** A Terraform fact's id is
   `res:{repo}@{rootdir}/{address}`, where `{rootdir}` is the repo-relative
   directory of the root module, with `/` separators. A root module at the
   repository root has no prefix: its ids stay `res:{repo}@{address}`, so a
   repository whose Terraform all lives at the root keeps identical ids and
   an identical graph hash.
2. **Root module vs. child module.** A root module is a directory whose
   `.tf` files the walk reads and that is not the resolved source of a local
   module call (AC-0048) anywhere in the repository. A local module's
   resources are instantiated under the scope of the root module that calls
   it, after the `module.<name>.` prefix: `res:{repo}@envs/prod/module.queue.aws_sqs_queue.this`.
   The walk also reads a child module's directory directly; that read is
   scoped by the child's own directory (`modules/queue/aws_sqs_queue.this`),
   so it can no longer collide with a root module or with another child
   module, and it is never treated as a root module for any other purpose.
   A Terraform address never contains `/`, so within a T0 Terraform id the
   scope is everything after the first `@` up to the last `/`.
3. **Edges stay inside their root module.** Terraform references cannot
   cross root modules (cross-root reads go through
   `data.terraform_remote_state`, itself a node in the reading root), so
   every `DEPENDS_ON`, `REFERENCES`, `GRANTS` and Capability Registry edge
   target takes the scope of the block that declares it. The extractor
   threads the scope beside `address_prefix` into `resource_id`, and the
   per-file cache key includes it.
4. **`resource_id_from_node` reads the scope, not just the repo.** The
   policy-document chase rebuilds a `GRANTS` target from its source node's
   id; it must keep that node's `{rootdir}/` scope, or a chased grant in
   `envs/prod` would target the unscoped (root) resource. It splits the
   source id at the last `/` after `@`, and its test covers a scoped and an
   unscoped source.
5. **The Gap is retired.** With scoped ids two root modules cannot share an
   id, so the `gap:tf-address-collision:` Gap is no longer emitted.
6. **Other `res:` producers are unchanged.** Observed resources
   (`res:{repo}@observed:{provider}[:{name}]`, `adapters-fw/src/observed.rs`)
   and Pulumi resources (`res:{repo}@pulumi:{type}:{name}`,
   `adapters-lang-ts`) keep their ids. Their identities may contain `/`
   (an `http-api` origin), so no consumer may parse a scope out of a
   non-Terraform `res:` id; consumers read `logical_id` and the
   `root_module` prop (absent for the repository root) instead. Two Pulumi
   projects in one repository can collide the same way; scoping them by
   the directory of their `Pulumi.yaml` is a separate decision, recorded
   here as not covered.

### Impact

- **`iac`:** `resource_id`, `extract_source_unresolved`, the module
  expansion, `cached_file_key` and `resource_id_from_node` gain the scope.
  `logical_id` stays the Terraform address. A node scoped to a root module
  outside the repository root carries `root_module: "<rootdir>"` (for
  example `"envs/prod"`), on its own resources and on the child-module
  resources it instantiates. A node of the repository-root module carries
  no `root_module` prop: absence means the repository root, and the prop
  never holds `"."` or an empty string. Root-only repositories therefore
  keep byte-identical nodes and an identical graph hash. Nodes from a direct
  read of a child module's directory carry `child_module: "<dir>"` and no
  `root_module`; no state file matches them.
- **Manifest: which root module a state file belongs to.** Today
  `ManifestRepo` (`ingest/src/manifest.rs`) has one unscoped `state_json`
  per repository, and `terraform show -json` output names no directory, so
  the association must be declared. The manifest gains an optional list on
  the repo entry:

  ```toml
  [[repos]]
  url = "acme/infra"
  state_json = "state/root.json"            # repository-root module, as today
  [[repos.terraform_state]]
  path = "state/prod.json"                  # relative to the manifest
  root_module = "envs/prod"                 # repo-relative, "/" separators
  [[repos.terraform_state]]
  path = "state/staging.json"
  root_module = "envs/staging"
  ```

  `state_json` keeps its meaning: it is the state of the repository-root
  module. Each `terraform_state` entry names one root module by its
  repo-relative directory. A `root_module` that is empty, `"."`, absolute,
  contains `..` or ends in `/` is a manifest load error, like an invalid
  base URL; the repository root is named only by using `state_json`. The
  association is resolved after T0 extraction, deterministically, with no
  silent skip:

  - **Declared root module exists:** the state matches only nodes whose
    `root_module` equals it (for `state_json`, only nodes with neither a
    `root_module` nor a `child_module` prop).
  - **Declared directory is not a root module** (no `.tf` files read
    there, or it is only ever a local module's resolved source): a
    `gap:tf-state-root:{repo}@{root_module}` Gap citing the manifest entry
    and the state file; the state is not applied.
  - **Two declarations for one root module** (two entries with the same
    `root_module`): a `gap:tf-state-root:{repo}@{root_module}` Gap citing
    both paths; neither is applied, because nothing says which is current.
  - **`state_json` declared but the repository has no repository-root
    module** (all Terraform lives in subdirectories): a
    `gap:tf-state-root:{repo}@` Gap that lists the root modules found and
    says to declare `terraform_state` entries; the state is not applied.
  - **A root module with Terraform resources and no state declared:**
    reported by coverage as today's `state_json` stopping reason, once per
    root module, naming the `terraform_state` field.

  A state address that matches no node of its root module is left
  unmatched, as today.
- **Observed-state matching (`dynamic::enrich_resources`, AC-0009):** it
  strips `res:{repo}@` and looks the remainder up as a state address. With
  the association above, matching for a scoped state strips
  `res:{repo}@{rootdir}/` and considers only nodes whose `root_module`
  equals `{rootdir}`; matching for `state_json` considers only nodes with
  neither prop and strips `res:{repo}@`, exactly as today. Observed
  resources from `adapters-fw/src/observed.rs` (line 235 builds
  `res:{repo}@observed:…`) keep matching by their own ids; they never match
  a Terraform node by address, and the scope rule must not make them start.
  Pulumi enrichment (AC-0052) is unaffected.
- **`spec` diagrams (`workbench/diagrams.rs`):** the topology label is
  `logical_id`, which stays the bare address, so two root modules would
  draw two boxes with the same text. The label shows `{rootdir}: {address}`
  when `root_module` is present; Mermaid ids already derive from the full
  node id and stay distinct.
- **App:** prefix filters on `res:{repo}@` (`src-tauri/src/main.rs`) keep
  working because the repository prefix is unchanged.
- **Graph hash:** one-time change for every repository with Terraform
  outside the repository root: every scoped resource and every edge that
  names one gets a new id. Repositories with Terraform only at the root are
  byte-identical. Accepted drafts and citations that name an old id become
  stale through the normal stale-citation path; nothing is migrated in
  place. The PR that implements this is an owner-review change (fact
  identity, graph hash).

### Alternatives

1. **Keep the Gap only (status quo after #1844).** No identity change, but
   one declaration per collision stays lost, and D7 §7.2 cannot list a
   Terraform environment's resources.
2. **Scope every id, including the repository root (`./{address}`).** One
   uniform rule, but it changes the hash and every id for every Terraform
   repository, for no gain where nothing collides.
3. **Merge colliding declarations into one node with several evidence
   spans.** Keeps ids, but asserts the two environments' resources are one
   thing, which the source does not state, and their attributes differ.
