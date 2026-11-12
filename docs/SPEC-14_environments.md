# SPEC-14 — Environments and what they declare

- **Status:** Proposed for [#1791](https://github.com/true-line/cartograph/issues/1791); increment 1 (Compose) implemented; owner review of ADR-0070 pending.
- **Date:** 2026-11-12
- **Scope:** US-0019 / AC-0661; `docs/DOCUMENT-CATALOGUE.md` D7 §7.2.
- **Decision:** [ADR-0070](adr/ADR-0070-environment-facts.md).

## Outcome and boundary

`environments.md` lists each environment the registered sources declare, the
workloads each declares, and the source span of every statement. Increment 1
reads Compose files. It states what it does not read.

Not in this specification: live cloud state; which environment is
production; the effective configuration after Compose files are combined;
Terraform resources per environment (#1844 first); secrets or any value of
an environment key.

## Facts (increment 1)

Extractor `t0.environments`, T0, in `crates/ingest`. YAML is parsed with the
span-preserving parser the schedule and pipeline readers use.

**Files read:** `compose.yaml`, `compose.yml`, `docker-compose.yaml`,
`docker-compose.yml`, and the same with one infix (`compose.*.yaml`,
`docker-compose.*.yml`, and so on), at any depth, under the walk's usual
exclusions.

| Fact | Identity | Properties | Confidence |
|---|---|---|---|
| `Environment` | `env:{repo}@{path}` | `declared_by: "compose"`, `path`, `name` and `name_span` only when the file has a top-level scalar `name` | Confirmed; span of the `services` key |
| `Workload` | `workload:{repo}@{path}#{service}` | `kind: "compose-service"`, `name`, and where present: `image`, `build` (context path only), `ports` (as written), `environment_keys` (names, sorted), `env_files` (paths), `depends_on` (service names, sorted), `profiles` | Confirmed; span of the service's mapping |
| `DECLARES` | `Environment` → `Workload` | — | Confirmed; span of the service key |
| `DEPENDS_ON` | `Workload` → `Workload` in the same file | — | Confirmed; span of the `depends_on` entry |
| `Gap` | `gap:environment:{repo}@{path}[#{service}]` | `kind` (`compose-file`, `compose-service`, `name-only-environment`), `path`, `service` where it applies, `reason`, `attempted_tiers: ["T0"]` | Gap; span of each construct, except a name-only environment Gap, which stores no span (AC-0097: no byte range of an environment file is stored) |

`Workload` also carries `path`. There is one Gap per file or service: several
constructs in one service share it, with their reasons joined and one span
each. The new Gaps appear in the gap register like any other.

**Values.** `environment` in list form (`KEY=value`) and map form yields key
names only. `image` and `ports` scalars pass through redaction; a
token-shaped scalar is replaced by the withheld marker, which includes a long
registry path or an image digest. Long-form `ports` entries are stored as
`key=value` pairs of the known keys in file order. No other scalar of a
service is stored.

**Gaps and limits.**

- The file does not parse, or has no `services` mapping: one file Gap,
  no `Environment`.
- A service uses `extends`, an anchor, an alias or a tag, or the file uses
  top-level `include`: the service (or file) Gap states the construct; the
  properties that were written literally are still recorded.
- A scalar that contains `${`: stored as written and marked in an
  `interpolated` map on the fact (`{"image": true}`); it is not resolved. A
  variable written `$NAME` without braces is stored as written and not marked.
- A property whose shape the reader does not know, or a service name that is
  not a plain scalar: a Gap.
- A `depends_on` entry naming a service the file does not declare: a Gap,
  not an edge.
- **Name-only environments.** For each environment file the toolchain reader
  recognises whose name carries a non-template infix or prefix (for example
  `.env.production`, `staging.env`; not `.env`, `.env.example`,
  `env.sample`, `env.template`; only the words `example`, `sample` and
`template` count as templates, so `.env.local` and `.env.test` are included),
one Gap
  `gap:environment:{repo}@{path}` with the reason "an environment is named
  only by this file's name; no file declares it". No `Environment`.

A Compose file whose name the toolchain reader accepts and this reader does
not (two infixes, or a hyphenated suffix such as `docker-compose-dev.yml`)
keeps its `Tool` fact and yields no `Environment` and no Gap.

**Coverage.** The reader is a producer in the coverage inventory with its
file, node and edge counts; its file count is Compose files read plus
name-only environment files. A Kubernetes manifest is recognised by a parsed
YAML document with top-level scalar `apiVersion` and `kind` (not a `batch/*`
CronJob); kustomize files and `Chart.yaml` by name; a Helm values file only
beside a `Chart.yaml`. Recognising a manifest opens YAML files that no name
rule selects, to read those two keys and nothing else; a file over 1 MiB is not opened. Kubernetes manifests (other than the `CronJob`
schedules already read), `kustomization.yaml`, Helm charts and values files
found by Preflight are recorded as unsupported capabilities with the reason
"present and not read: no installed producer recovers environments from it".

**Determinism.** Files are walked in sorted order; lists that the file does
not order semantically (`environment_keys`, `depends_on`) are sorted; the
same commit gives the same facts.

## Facts (increment 2: Kubernetes manifests and kustomize)

Same extractor and parser. Helm is still not read.

**Manifests.** A YAML document with top-level scalar `apiVersion` and `kind`
is a manifest document. Of these, the pod-bearing kinds `Deployment`,
`StatefulSet`, `DaemonSet` and `Job` are read. Every other kind (`Service`,
`Ingress`, `ConfigMap`, `Secret`, custom resources and the rest) is recorded
in coverage as present and not read, by kind, and no longer as a whole file.
`CronJob` stays with the schedule reader.

| Fact | Identity | Properties | Confidence |
|---|---|---|---|
| `Workload` | `workload:{repo}@{path}#{document index}:{Kind}/{metadata.name}` | `kind: "k8s-deployment"` (and `-statefulset`, `-daemonset`, `-job`), `name`, `path`, `namespace` only when `metadata.namespace` is stated, `replicas` when a literal integer, `containers`: for each entry of `spec.template.spec.containers` and `initContainers` in file order, `name`, `image`, `ports` (`containerPort` values), `environment_keys` (names from `env[].name`, sorted), `env_from` (the names of referenced ConfigMaps and Secrets, never their content) | Confirmed; span of the document |
| `Environment` | `env:{repo}@{path}` for each `kustomization.yaml`, `kustomization.yml` or `Kustomization` | `declared_by: "kustomize"`, `path`, `name` and `name_span` only when the file states a scalar `namespace`, `image_overrides` (each `images` entry as written: `name`, `newName`, `newTag`, `digest`), `includes` (the `resources` and `bases` entries that are kustomization directories in the repository, as paths) | Confirmed; span of the `resources` key, or of the file's first key when it has none |
| `DECLARES` | `Environment` → `Workload` | `via` (the chain of kustomization paths from this environment to the file, empty when direct) | Confirmed; span of the `resources` entry in this environment's own file that leads to the workload |
| `PATCHES` | `Environment` → `Workload` | `path` (the patch file), and the properties the patch document writes for that workload, in the `Workload` property shape (`replicas`, `containers` with only what the patch states) | Confirmed; span of the patch document |

**Reach.** An environment declares the workloads in the manifest files its
`resources` (and legacy `bases`) entries name, and in the kustomization
directories they name, followed recursively inside the repository. A cycle
ends the walk on that branch with a Gap. Depth is bounded at 16 with a Gap
beyond it. A workload that several environments reach has one `DECLARES`
edge from each.

**Patches.** A `patches[].path` or `patchesStrategicMerge` entry naming a
file in the repository is read as manifest documents. A document whose
`kind` and `metadata.name` match a workload the environment reaches yields
`PATCHES`. Nothing is merged: the base workload keeps its properties, and
the patch states its own.

**Gaps and limits (one Gap per kustomization, reasons joined, one span each).**

- A `resources`, `bases` or patch entry that is a URL, a remote git
  reference, or a path outside the repository or not found.
- A patch document that matches no reached workload, an inline patch, a
  `patches[].target` selector, and `patchesJson6902`.
- Transformers and generators that are present and not evaluated:
  `namePrefix`, `nameSuffix`, `commonLabels`, `labels`, `replicas`,
  `configMapGenerator`, `secretGenerator`, `components`, `helmCharts`,
  `generators`, `transformers`, `replacements`, `vars`. The Gap names the
  keys. `namePrefix` and `nameSuffix` mean the deployed name differs from the
  recorded one; the document says so on that environment's rows.
- A manifest document of a read kind without a scalar `metadata.name`, or
  whose `containers` is not a sequence of mappings: a Gap for that document.
- A file that does not parse (which includes a Helm template) is not a
  manifest and yields nothing here; if it is named by a kustomization it is
  a Gap there.
- Anchors, aliases and tags in a read document: a Gap, and the literal
  properties are still recorded.

**Values.** No `env[].value`, no `data` or `stringData` of any document, and
no `valueFrom` content is stored. `image` and port scalars pass through
redaction. `configMapGenerator` and `secretGenerator` literals are never
read.

**Workloads no environment declares.** A manifest workload that no
kustomization reaches is still a `Workload`; it has no `DECLARES` edge.

## The document

`environments.md`, artifact id `environments`, in both export modes:

1. What an environment is here (a declared deployable set, shown by path;
   a name only where the file states one), and what is not read.
2. Counts: environments, workloads, name-only Gaps, other Gaps.
3. **Environments:** one row per `Environment`: path, stated name or "none
   stated", declared by, workload count, span.
4. **Workloads by environment:** per environment, one row per workload:
   name, image, ports, environment key names, dependencies, profiles, span.
5. **Declared in more than one environment:** for each workload name that
   two or more environments of one repository declare, one row per
   declaration with the properties that differ and each span. The section
   does not say which applies where.
6. **Not established:** the Gaps, with reason and span.

With increment 2: the workload table shows one row per container for a
manifest workload; section 5 also lists a workload that two or more
environments reach, one row per environment, with what that environment's
`PATCHES` and `image_overrides` state for it, or "no patch stated"; a new
section **Workloads no environment declares** lists manifest workloads with
no `DECLARES` edge; an environment that another includes shows "included by"
with the including paths.

Every row carries tier and confidence and cites its fact through the fact
index (ADR-0058). `verified-only` omits InferredWeak rows with a count
(R-INT-5); increment 1 produces none. Work packets refuse the id until a
decision says otherwise.

## Increments

1. **Compose** (this specification).
2. Kubernetes manifests and kustomize (specified above; AC-0662).
3. Helm values files and per-environment settings files, if a file states
   the environment; otherwise they stay Gaps.
4. Terraform root modules, after #1844.

AC-0661 covers increment 1 and AC-0662 increment 2. The issue's evidence (a
Compose file and a Kubernetes overlay for two environments) is complete
after increment 2.

## Open items

- Manifest recognition reads two keys of every YAML file that is not a
  Compose file or a workflow. US-0019 says only name-matched configuration
  files are read; this is the one exception and it stores nothing from them.
- Helm templates that are not valid YAML are not recognised as manifests.

- Whether a `Workload` should appear in `topology.md`.
- Linking a `Workload`'s image to the Dockerfile pipeline that builds it
  (ADR-0060).
- Linking `env_files` to the environment-file `Tool` facts.
