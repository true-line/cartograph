# Document catalogue

- **Status:** Version 1.0. Approved by the owner on 2026-11-17 (#1856), as drafted in 0.2 (#1747) with no mark-up. A change to this catalogue is a new version and needs the owner's approval.
- **Date:** 2026-11-17 (drafted 2026-11-09)
- **Direction:** [ADR-0056](adr/ADR-0056-documentation-reconstruction-pipeline.md) decision 3, [VISION](VISION.md) V10, [ROADMAP](ROADMAP.md) §10 (W14)

This is the list of documents Cartograph produces for an ingested system: the
set a well-run team would have kept from day one. For each document it says
who reads it, what each section contains, where the content comes from, which
sections an agent may write, and how the document is accepted.

It decides *what* is produced. How a section is drafted and reviewed is
#1748; where an accepted section becomes visible is #1746; how a section is
kept current is #1749. Nothing here is built by this document.

## 1. Rules for every document

**Where a section's content comes from.** Each section has exactly one source
kind:

| Kind | Meaning | On re-ingest |
|------|---------|--------------|
| **P** — projected | Compiled by rule from graph facts. No model is involved. | Regenerates |
| **A** — authored | Drafted by an agent from cited facts, staged, and shown only after a human accepts it. Stays labelled T3 / InferredWeak and agent-authored after acceptance. | Marked stale when its cited facts change (#1749) |
| **P+A** — framed | A projected frame (tables, diagrams, lists) with authored prose around it. The prose never replaces or contradicts a projected statement. | Frame regenerates; prose is marked stale |
| **H** — human | Something the code cannot show (business intent, contracts, staffing, cost). Emitted as a labelled blank for the team to fill. Cartograph never invents it. | Kept |

**Every document carries:**

1. A header: system name, source revisions, graph content hash (#1765),
   catalogue version, export mode, and a count of statements by tier.
2. Tier and confidence on every statement, and an author label on every
   authored section (R-INT-2).
3. A closing "Known gaps" section projected from the gap register, scoped
   to that document's subject.
4. A list of its stale sections, once #1749 exists.

**Export modes.** The confidence filter applies to every statement, whatever
kind of section it is in. `verified-only` omits every InferredWeak statement:
all A sections, the prose of P+A sections, and any InferredWeak statement
inside a P section (a T2 journey candidate in D9 §9.1, for example). It
states how many statements were omitted, per section. `best-effort` includes
them, annotated (R-INT-5).

**Acceptance.** A P section is accepted by a deterministic check against a
fixture. An A or P+A section is accepted by content acceptance: an expected
set written from the target before the run, kept out of every model input,
and scored claim by claim by someone who did not produce the draft.

## 2. The document set

### D1 — Product specification

**Reader:** product owners, engineers, anyone asking "what does this system
do, for whom, under which rules".

| # | Section | Kind | Source today |
|---|---------|------|--------------|
| 1.1 | Purpose and scope of the system | A | Capabilities, journeys, README and other target documents (#1750) |
| 1.2 | Actors and roles | P+A | `Actor` facts and permission sites; prose explains each role |
| 1.3 | Capabilities | P+A | `Capability` facts (`user_stories.md`); prose states each as a user story |
| 1.4 | Acceptance criteria per capability | P+A | Rule evidence, recovered responses, test assertions; prose writes Given/When/Then, each clause cited |
| 1.5 | Business rules | P+A | `rule-evidence.md` observations and branch conditions; prose states the rule in domain language |
| 1.6 | Business flows | P | `BusinessFlow` facts, flow dossiers |
| 1.7 | Out of scope and non-goals | H | — |
| 1.8 | Known gaps | P | Gap register |

**Acceptance:** content acceptance on one SimpleLogin capability set (#1748
requirement 8); P sections by fixture.

### D2 — Architecture overview

**Reader:** engineers and architects.

| # | Section | Kind | Source today |
|---|---------|------|--------------|
| 2.1 | System context: what it is, who and what it talks to | P+A | Clients, external endpoints, cross-repo stitching |
| 2.2 | Components and repositories | P | Repositories, layers, coverage inventory |
| 2.3 | Infrastructure and the role each resource plays | P+A | `topology.md` (`Resource`, `Channel`); prose explains each role in the flows that use it |
| 2.4 | Runtime flows | P | `flow_dossiers.md` |
| 2.5 | Events and messaging | P | Channels, producers, consumers |
| 2.6 | Cross-cutting concerns: authentication, configuration, errors | A | Permission sites, toolchain facts, rule evidence |
| 2.7 | Quality attributes and constraints | H | — |
| 2.8 | Known gaps | P | Gap register |

**Acceptance:** P sections by fixture; 2.1, 2.3 and 2.6 by content acceptance.

### D3 — Architecture decision records

**Reader:** engineers.

| # | Section | Kind | Source today |
|---|---------|------|--------------|
| 3.1 | ADRs found in the target, as written | P | `adrs.md` (origin: found) |
| 3.2 | Recovered decisions: what the code shows was decided | P | `adrs.md` (origin: recovered), `DECIDES` edges |
| 3.3 | Context and consequences for each recovered decision | A | The decision's cited facts and its drift findings |
| 3.4 | Rationale: why it was decided | H, or A where a target document states it | Target documentation (#1750) |
| 3.5 | Decisions the code violates | P | Drift register (ADR findings) |

A recovered decision states what the code does. Its rationale is written
only where a source states it; otherwise the field is a labelled blank.

**Acceptance:** 3.1, 3.2 and 3.5 by fixture; 3.3 by content acceptance.

### D4 — API reference

**Reader:** integrators and engineers.

| # | Section | Kind | Source today |
|---|---------|------|--------------|
| 4.1 | Endpoint inventory | P | `Endpoint` facts, projected as `endpoint-inventory.md` (#1658) |
| 4.2 | Per endpoint: method, path, parameters, responses, authentication | P | Handlers, recovered responses, permission sites |
| 4.3 | Per endpoint: what it is for | A | Handler, rule evidence, tests that reference it |
| 4.4 | Differences from the target's own API documentation | P | Drift register (contract findings) |
| 4.5 | Events published and consumed | P | Channels |

**Acceptance:** 4.1, 4.2 and 4.4 against the SimpleLogin contract (44 of 45
endpoints today); 4.3 by content acceptance.

### D5 — Data dictionary

**Reader:** engineers, data and compliance.

| # | Section | Kind | Source today |
|---|---------|------|--------------|
| 5.1 | Entity diagram | P | `data_model.md` |
| 5.2 | Per entity: fields, types, relations, storage | P | `DataEntity` facts and mappings; `data_model.md` prints a citation id for each (#1785) |
| 5.3 | Per entity: what it represents in the business | A | `DomainEntity` facts, the flows that read and write it |
| 5.4 | Which flows create, read, update and delete each entity | P | Flow dossiers |
| 5.5 | Schema drift between model and migrations | P | Drift register |
| 5.6 | Personal and sensitive data classification | A, proposed for human confirmation | Field names, permission sites |

**Acceptance:** P sections by fixture; 5.3 by content acceptance; 5.6 is
always shown as unconfirmed until a human confirms each row.

### D6 — Security and compliance overview

**Reader:** security and audit.

| # | Section | Kind | Source today |
|---|---------|------|--------------|
| 6.1 | Authentication and authorization model | P+A | Permission sites, `Actor` caller sets |
| 6.2 | Permissions granted to infrastructure | P | `security.md` (IAM findings) |
| 6.3 | Endpoints with no recovered authentication check | P | `endpoint-permissions.md`: endpoints against the permission sites recovered for them (Flask, Flask-Admin, Spring, Go `net/http`, chi and gin, and NestJS GraphQL today) |
| 6.4 | Secrets and configuration handling | P | `configuration.md`: key names declared in environment files and read in source (never values), and toolchain facts |
| 6.5 | External data flows | P | `external.md`, "External data flows": one row for each flow and outside HTTP origin it reaches, with the entry point and the data entities read or written on that flow (SPEC-13, ADR-0063) |
| 6.6 | Compliance obligations | H | — |

**Acceptance:** P sections by fixture; 6.1 prose by content acceptance.

### D7 — Operations and deployment runbook

**Reader:** operations and on-call engineers.

| # | Section | Kind | Source today |
|---|---------|------|--------------|
| 7.1 | Build and toolchain | P | `toolchain.md` |
| 7.2 | Environments and deployed resources | P | `environments.md`: each Compose file and each kustomization file as one environment, shown by its path and named only where the file states a name (a Compose `name`, a kustomize `namespace`), with the workloads it declares: Compose services (image, build context, ports, environment key names, environment files, dependencies, profiles) and the `Deployment`, `StatefulSet`, `DaemonSet` and `Job` manifests a kustomization's `resources` chain reaches, one row per container; each workload name that several environments of one repository declare with the properties that differ and each span; each Kubernetes workload that several kustomizations reach, once per environment with what that environment's patch files and `images` entries state, nothing applied; Kubernetes workloads no environment declares; and what is not established (increments 1 and 2 of SPEC-14, ADR-0070); infrastructure facts and observed state where supplied |
| 7.3 | Configuration: every key the system declares or reads | P | `configuration.md`: each key once per repository, with the environment files that declare it (`Tool` facts, category `environment`) and the files and symbols that read it (`ConfigRead` facts: standard-library environment reads in TypeScript/JavaScript, Python, Go, Java, Kotlin and Rust; ADR-0059), keys read and never declared, keys declared and never read, and the files with reads it cannot name; never values |
| 7.4 | Scheduled jobs | P | `Schedule` flow triggers (yacron, crontab, Kubernetes `CronJob`) |
| 7.5 | How to deploy | A | `pipelines.md`: each pipeline with its triggers, its jobs in dependency order and their steps (GitHub Actions workflows and Dockerfiles; ADR-0060), `Tool` facts, target documentation |
| 7.6 | Dependencies on external services | P | `external.md`, "External services": each outside HTTP origin with its call sites and spans, and the unclassified targets (run-time or configured hosts, SDK and auth-provider clients, published channels with no subscriber or backing resource) with their reasons (SPEC-13, ADR-0063) |
| 7.7 | Alerts, on-call and recovery procedures | H | — |

**Acceptance:** P sections by fixture; 7.5 by content acceptance.

### D8 — Test strategy and coverage

**Reader:** QA and engineers.

| # | Section | Kind | Source today |
|---|---------|------|--------------|
| 8.1 | Test suites and frameworks found | P | `test_arrangements.md` |
| 8.2 | Which tests reference which endpoints and flows | P | Test references (never claimed as executed coverage) |
| 8.3 | Endpoints and flow steps no test references | P | Test arrangements |
| 8.4 | Test and contract drift | P | Drift register (test findings) |
| 8.5 | Strategy: what the suite is evidently designed to protect | A | 8.1 to 8.4 |

**Acceptance:** P sections on SimpleLogin's test suite; 8.5 by content
acceptance.

### D9 — User journeys

**Reader:** product and design.

| # | Section | Kind | Source today |
|---|---------|------|--------------|
| 9.1 | Journey candidates with steps | P | `journeys.md` (T0 navigation facts, T2 candidates) |
| 9.2 | Flows and tests behind each step | P | `journeys.md` |
| 9.3 | Assumptions, truncation and alternatives | P | `journeys.md` |
| 9.4 | Journey narrative: goal, actor, outcome | A | 9.1 to 9.3 |

**Acceptance:** one SimpleLogin journey scored against an expected set
written from its public documentation (ROADMAP W11).

### D10 — Onboarding guide and glossary

**Reader:** engineers new to the system.

| # | Section | Kind | Source today |
|---|---------|------|--------------|
| 10.1 | Where things are: repositories, layers, entry points | P | Repositories, coverage inventory |
| 10.2 | How to build and run locally | P+A | Toolchain facts, target README (#1750) |
| 10.3 | Glossary of domain terms | A | `DomainEntity`, `Capability` and `Actor` names with their definitions cited |
| 10.4 | A first reading path through the system | A | Capabilities and the flows behind them |

**Acceptance:** 10.1 by fixture; 10.3 by content acceptance.

### D11 — Evidence appendices

**Reader:** anyone checking a statement in D1 to D10.

| # | Appendix | Kind | Source today |
|---|----------|------|--------------|
| 11.1 | Traceability matrix | P | `US-TM.md`, `US-TM.json` |
| 11.2 | Gap register | P | `gap_register.md`, `gap_register.json` |
| 11.3 | Drift register | P | `drift_register.md` |
| 11.4 | Rule evidence | P | `rule-evidence.md`, `rule-evidence.json` |
| 11.5 | Flow dossiers, complete | P | `flow_dossiers.md` |
| 11.6 | Coverage: what was read, skipped and why | P | Coverage inventory |
| 11.7 | Fact and evidence index: every fact a document cites, with its tier, confidence, method and source span | P | `fact-index.md`, `fact-index.json` (ADR-0058) |

No authored content. Every statement in D1 to D10 must resolve to a row in
§11.7. The traceability matrix cannot carry that: it indexes only the
seven trace relations (`REALIZES`, `STEP_OF`, `GOVERNS`, `MAPS_TO`,
`DECIDES`, `TRIGGERED_BY`, `PERFORMED_BY`), so an endpoint's method or a
field's type has no row there. §11.7 lists every fact an exported artifact
asserts, once, under a citation id that names the fact and not its place in
a document (ADR-0058). Compiling an export fails if a cited id has no row.
The existing documents do not print the id beside each statement yet; that
is added one document at a time. Until a document prints its citations, it
may only make statements whose evidence one of §11.1 to §11.5 already holds.

## 3. Where today's thirty artifacts go

| Artifact | Becomes |
|----------|---------|
| `user_stories.md` | D1 §1.2, §1.3 |
| `US-TM.md`, `US-TM.json` | D11 §11.1 |
| `flow_dossiers.md` | D2 §2.4, D1 §1.6, D11 §11.5 |
| `topology.md` | D2 §2.3 |
| `data_model.md` | D5 §5.1, §5.2 |
| `adrs.md` | D3 §3.1, §3.2 |
| `gap_register.md`, `gap_register.json` | D11 §11.2, and each document's "Known gaps" |
| `drift_register.md` | D11 §11.3, D3 §3.5, D4 §4.4, D5 §5.5, D8 §8.4 |
| `security.md` | D6 §6.2 |
| `toolchain.md` | D7 §7.1, §7.3 |
| `test_arrangements.md` | D8 §8.1 to §8.3 |
| `journeys.md` | D9 §9.1 to §9.3 |
| `rule-evidence.md`, `rule-evidence.json` | D1 §1.5, D11 §11.4 |
| `endpoint-permissions.md` | D6 §6.3 |
| `pipelines.md` | D7 §7.5 |
| `configuration.md` | D7 §7.3, D6 §6.4 |
| `external.md` | D7 §7.6, D6 §6.5 |
| `environments.md` | D7 §7.2 |
| `documentation-evidence.md` | No section of its own. It lists the target's documentation files, their sections and the facts each names (SPEC-18, ADR-0065); D1 §1.1, D3 §3.4 and D10 §10.2 draw on those sections through `documented_intent` claims |
| `architecture.md`, `architecture.dsl` | D1 §1.1 (the system at a glance), D7 §7.1; C4 levels 1 to 3 drawn from the recovered facts, with a Structurizr DSL workspace of the same model (#2049) |
| `curated.md` | No section of its own. It lists the accepted proposals and their states (SPEC-12, ADR-0061); the A and P+A sections draw on them once drafting exists (#1748) |
| `coverage.md`, `coverage.json` | D2 §2.2, D10 §10.1, D11 §11.6 |
| `bundle-manifest.md` | No section of its own. It states the export's as-of and lists every exported file under its handoff-bundle part (SPEC-16, AC-0682) |
| `fact-index.md`, `fact-index.json` | D11 §11.7 |

The thirty files keep being exported under their current names until each
target document exists, so nothing a current consumer reads disappears.

## 4. Sections with no source today

These need new deterministic work before the section can be produced. Each
row names the issue that tracks it.

| Section | What is missing |
|---------|-----------------|
| D1 §1.1, D3 §3.4, D7 §7.5, D10 §10.2 | Target documentation is recovered as `Document` and `DocSection` facts and projected as `documentation-evidence.md` (#1750, SPEC-18). What a section says is not stored: a draft cites the section, and a reader opens the file |
| D1 §1.2, §1.3, D5 §5.3, D10 §10.3 on a real repository | The knowledge phase now reads a base past 50,000 rows, up to the ADR-0052 ceiling (#1771, fixed). A skip past the ceiling is still not shown in the app (#1782) |
| D4 §4.1, §4.2 | Endpoints are projected as `endpoint-inventory.md` (#1658, fixed): every recovered endpoint with method and path as recorded or unknown, and tier, confidence and span. Its "Requests and responses" section (#1998) states, per endpoint, the literal input names the handler reads, the statuses, body kinds and top-level JSON keys it returns and its flask-limiter rate limit, each cited. These are recovered for Flask handlers only; other frameworks read "not recovered". JSON keys are not split by status, nested shapes and types are not recovered, and authentication stays in `endpoint-permissions.md` |
| D6 §6.3 | Endpoints are projected against permission sites as `endpoint-permissions.md` (#1787, fixed). Permission sites are recovered for Flask, Flask-Admin, Spring, NestJS GraphQL and Go `net/http`, chi and gin only. Go guards are read from route middleware (#2163) and a Go route with none is never "none recovered"; the others only from the endpoint's own declaration: an endpoint of any other framework is listed under a coverage note in neither table, and a check made by middleware, a gateway or code inside a handler body is not recovered (#1320 covers Flask in-body checks). "None recovered" therefore never means that no check runs |
| D6 §6.5, D7 §7.6 | Calls and publishes that leave the system boundary are projected as `external.md` by one predicate (#1788, fixed; ADR-0063, SPEC-13): each literal HTTP origin no declared binding covers, with its sites, the flows that reach it and the entities read or written on them; unclassified targets with their reasons; and a count of calls binding evidence places in a registered repository. The outside list is a lower bound. The binding pass records its outcome on each call and observed HTTP client it evaluates (#1843), so in a system with declared bindings a third-party call is outside and a client of a declared host is not; a system ingested before that keeps such calls unclassified until it is ingested again. Still not covered: operators of SDK providers; inbound channels and webhooks; what a request body contains |
| D7 §7.2 | Environments are recovered from Compose files, kustomization files and Kubernetes manifests and projected as `environments.md` (#1791, increments 1 and 2; ADR-0070, SPEC-14): one environment for each Compose file and each kustomization file, shown by path and never named from it; one workload for each Compose service and each `Deployment`, `StatefulSet`, `DaemonSet` and `Job` document; a kustomization declares the workloads its `resources` chain reaches inside the repository, and a patch file is shown as what it writes; a Gap for an unreadable file, a construct that is not evaluated (`extends`, `include`, anchors, aliases, tags, kustomize transformers and generators, inline and JSON patches, `target` selectors), a dependency on an undeclared service, a kustomization entry that is remote, missing or outside the repository, a patch that matches nothing, and an environment file that names an environment only by its file name. Still not covered: Helm charts and Helm values files and per-environment settings files (increment 3; Helm files and manifests of other kinds are named in coverage as present and not read); Terraform root modules are not attached to an environment (#1844 first); nothing is merged or applied, so the effective configuration of a deployment is not shown; nothing says which environment is production; no value of an environment key is stored |
| D7 §7.3, D6 §6.4 | Every compiled-in adapter (TypeScript/JavaScript, Python, Go, Java, Kotlin, Rust) emits a `ConfigRead` fact for a standard-library environment read, named for a literal key and unnamed for a dynamic one (#1789, ADR-0059), and the keys are projected as `configuration.md` (#1821, fixed): each key once per repository with where it is declared and read, each with tier, confidence and span, and the files with reads it cannot name. Still not covered: `import.meta.env` and destructured `process.env`, `"X" in os.environ`, Rust `env!`, and reads through configuration frameworks (Spring `@Value`, NestJS `ConfigService`, Viper); only environment files count as declarations, so a key set in a compose file, a manifest, a workflow or a shell script is listed as never declared; an environment file has no span. There is no C# adapter. The sections themselves are not drafted yet (#1748) |
| D7 §7.5 | The ordered build and deployment steps of GitHub Actions workflows and Dockerfiles are recovered as facts (#1790, ADR-0060) and projected as `pipelines.md` (#1818, fixed): triggers, jobs in dependency order and steps in declared order, each with tier, confidence, span and the citation id of each fact it states (#1785). Still not covered: a Dockerfile with a heredoc and a workflow with a YAML alias, anchor or tag are listed as present and not read; GitLab CI, Jenkins, CircleCI and Azure Pipelines are named and not read; nothing is evaluated, so matrices, expressions, reusable workflows and third-party actions stay as declared. The coverage inventory holds a count and the first file for each kind of unread file, not every file. The section itself is authored and is not drafted yet (#1748) |
| D2 §2.2, D10 §10.1, D11 §11.6 | The coverage inventory is exported as `coverage.md` and `coverage.json` (#1786, fixed). It records each producer's file count, each missing input and each unsupported capability with its first site; it holds no list of every file found, read or skipped, so these sections cannot yet name each file |
| D11 §11.7 | The fact and evidence index is exported as `fact-index.md` and `fact-index.json` (#1785, fixed; ADR-0058). It has one row for each fact an exported artifact asserts, with its tier, confidence, method, evidence basis, extractor and spans, and the export fails if a cited id has no row. The existing documents do not yet print the citation id beside each statement, so D4 §4.2, D5 §5.2 and D7 become checkable statement by statement only as each document adds its citations. `curated.md`, the D1 document, `data_model.md` and `pipelines.md` print them; `pipelines.md` cites each pipeline, trigger, job, step, placing relation and need; `data_model.md` cites each entity and related node, each relation and each relation field, but it holds no field or type of an entity yet, so D5 §5.2 is checkable only for relations |
| Every header | The graph content hash is not available outside tests (#1765) |
| Every A and P+A section | Section drafting (#1748) on the curated projection (#1746) |

## 5. Defaults chosen in version 1.0

Each is a choice made to get a complete first version. The owner approved
them as written; changing one is a new catalogue version.

1. **Eleven documents.** Ten reader-facing documents and one set of evidence
   appendices. Candidates left out: release notes and changelog, cost and
   capacity, a UX and design-system inventory, and a migration or porting
   guide (the rebuild handoff, #1752, covers the last).
2. **Rationale is never invented.** A recovered ADR states what was decided
   and what follows from it. The "why" is written only where a target
   document states it (D3 §3.4). Commit messages are not a source: Git
   history is not read, and managed sources are shallow clones. Reading it
   would be a new fact source and is not proposed here.
3. **Human sections are emitted as labelled blanks**, so the document shows
   what the team still owes, and are never filled by a model.
4. **Sensitive-data classification (D5 §5.6) is proposed, never asserted.**
   Each row stays unconfirmed until a person confirms it.
5. **One file per document**, Markdown, with the appendices as separate
   files. Other formats are a rendering concern.
6. **The current twenty-six artifacts stay exported** alongside the new
   documents until every consumer has moved.
7. **Order of delivery:** D11 and the P sections of D4 and D5 first (no
   model needed), then D1, then D2 and D3, then the rest.
