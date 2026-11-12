# SPEC-14 — Environments and what they declare

- **Status:** Proposed for [#1791](https://github.com/true-line/cartograph/issues/1791); increment 1 (Compose) specified, not implemented.
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
| `Gap` | `gap:environment:{repo}@{path}[#{service}]` | `reason`, `attempted_tiers: ["T0"]` | Gap; span of the construct |

**Values.** `environment` in list form (`KEY=value`) and map form yields key
names only. `image` and `ports` scalars pass through redaction; a
token-shaped scalar is replaced by the withheld marker. No other scalar of a
service is stored.

**Gaps and limits.**

- The file does not parse, or has no `services` mapping: one file Gap,
  no `Environment`.
- A service uses `extends`, an anchor, an alias or a tag, or the file uses
  top-level `include`: the service (or file) Gap states the construct; the
  properties that were written literally are still recorded.
- A scalar that contains `${`: stored as written and marked
  `interpolated: true` on that property; it is not resolved.
- A `depends_on` entry naming a service the file does not declare: a Gap,
  not an edge.
- **Name-only environments.** For each environment file the toolchain reader
  recognises whose name carries a non-template infix or prefix (for example
  `.env.production`, `staging.env`; not `.env`, `.env.example`,
  `env.sample`, `env.template`), one Gap
  `gap:environment:{repo}@{path}` with the reason "an environment is named
  only by this file's name; no file declares it". No `Environment`.

**Coverage.** The reader is a producer in the coverage inventory with its
file, node and edge counts. Kubernetes manifests (other than the `CronJob`
schedules already read), `kustomization.yaml`, Helm charts and values files
found by Preflight are recorded as unsupported capabilities with the reason
"present and not read: no installed producer recovers environments from it".

**Determinism.** Files are walked in sorted order; lists that the file does
not order semantically (`environment_keys`, `depends_on`) are sorted; the
same commit gives the same facts.

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

Every row carries tier and confidence and cites its fact through the fact
index (ADR-0058). `verified-only` omits InferredWeak rows with a count
(R-INT-5); increment 1 produces none. Work packets refuse the id until a
decision says otherwise.

## Increments

1. **Compose** (this specification).
2. Kubernetes manifests and kustomize overlays: an overlay is an
   `Environment`; its `resources` chain gives its workloads; a patch is a
   second declaration.
3. Helm values files and per-environment settings files, if a file states
   the environment; otherwise they stay Gaps.
4. Terraform root modules, after #1844.

AC-0661 covers increment 1. The issue's evidence (a Compose file and a
Kubernetes overlay for two environments) is complete after increment 2.

## Open items

- Whether a `Workload` should appear in `topology.md`.
- Linking a `Workload`'s image to the Dockerfile pipeline that builds it
  (ADR-0060).
- Linking `env_files` to the environment-file `Tool` facts.
