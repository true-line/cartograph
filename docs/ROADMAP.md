# Roadmap — from structural recovery to lifecycle reconstruction

Status: proposed in [#572](https://github.com/true-line/cartograph/issues/572)
for owner review. Direction: [ADR-0036](adr/ADR-0036-lifecycle-reconstruction-direction.md).
Vision elements V1–V9: [VISION.md](VISION.md). Baseline: `main` at `cb0e876` (v0.15.5).

The decisions in section 3 were agreed between the two implementation agents
(Claude for core recovery and lifecycle artifacts, Codex for the agentic
layer). This document is the proposed roadmap built on them, under cross-review
in #574. It authorizes no implementation by itself: every item below lands
through its own scoped issue and pull request.

## 1. North star and how it is measured

Import a real multi-repo application, explain one feature with citations,
produce its journey, architecture and test bundle, review a gap, hand the gap
to an implementer as a work packet, re-ingest the change, and show which claim
and test changed and which evidence went stale.

Measured by time to a supported answer, precision and coverage with explicit
unknowns, source fidelity, repeatability, tokens, latency and cost, and whether
a user completes the cycle. Never by file, test or node counts.

## 2. Where the code is today

Implemented facts at the baseline commit, with the place to look.

**Intake and stitching (V1).**
- Intake is a typed path, a GitHub reference or a hand-written `cartograph.system.toml` (`ui/src/components/ConnectSurface.tsx:15-34`, dispatched at `ui/src/store.ts:1119`). No drop handler exists.
- A system is ingested repo by repo, then `stitch_backings` and `relink_found_adrs` run (`src-tauri/src/main.rs:3857-3864`). Channels join across repos only because channel ids carry no repo namespace (`crates/events/src/lib.rs:178`).
- Cross-repo HTTP is not stitched: `stitch_fetches` keeps only the calling repo's endpoints (`crates/events/src/lib.rs:294-300`), so a call to another repo's route ends as a Gap (`:343`).
- T1 observations (Terraform state, Pulumi export, OTel traces) come only from a manifest (`crates/ingest/src/manifest.rs:39-53`). EventBridge channel identities cannot match today ([#573](https://github.com/true-line/cartograph/issues/573)).

**Deterministic layer and derived meaning (V2, V3).**
- `core-prov` enforces a confidence ceiling per tier (`crates/core-prov/src/lib.rs:11-20`, `:95-142`). The producing method has no field of its own; `extractor_id` is a free string (`:85-86`).
- Nothing outside `crates/spec` produces `Capability`, `BusinessFlow`, `DomainEntity` or `Actor` nodes, so the user-stories artifact prints "No Capability facts have been recovered yet." on every real ingest (`crates/spec/src/workbench.rs:319-328`).
- The one rule-derived semantic producer is ADR recovery at `Tier::Semantic` (`crates/spec/src/adr.rs:274-347`).
- Omissions carry a stop reason, not a cause class (`crates/events/src/lib.rs:221-234`). Coverage is one prose `covers` sentence per adapter (`crates/ingest/src/preflight.rs:94-103`).

**Lifecycle artifacts and visualization (V4, V5).**
- The spec compiler emits 14 artifacts in a fixed order (`crates/spec/src/workbench.rs:1448-1460`). #239 records six of ten then-existing artifacts empty for a Spring app.
- The Vendure cart-readiness depth pilot scores 0/8 complete rules with all 41 decision cases unexecuted (`docs/evals/vendure-cart-readiness.local-definitions.score.md:3`).
- The flow tracer is a deterministic technical path engine (`crates/flowtracer/src/lib.rs:1-21`). "Journey" appears nowhere in the product.
- Topology and flow dossiers (`crates/spec/src/lib.rs:65-248`) and the data-model flowchart (`crates/spec/src/workbench.rs:709-798`) are Mermaid text shown as copyable text, not rendered (`ui/src/components/TopologyCard.tsx:13-30`). No test-case node exists.

**Agent team (V6).**
- Two named specialists, each at @1 and @2, with three operations at T3/InferredWeak (`crates/agents/src/investigation/specialists.rs:7-16`, `:94-100`).
- Four recorded real-provider attempts produced no source read and no finding (`docs/evals/investigation-local-discovery.md`). Each step re-serializes the whole ledger (`crates/agents/src/investigation/input.rs:163-181`).
- No MCP or ACP server, chat, pool, issue or work-packet producer exists in product code.

**Extensibility and skills (V7, V8, V9).**
- The WASM plugin contract is one function, `extract-source` (`crates/adapters-plugin-host/wit/adapter.wit:14-25`). A plugin can add a language but not augment one (`crates/ingest/src/preflight.rs:521-533`).
- Four registries are Rust `const` tables with no data or plugin entry point: event SDKs (`crates/adapters-fw/src/events.rs:78-81`), AWS capabilities (`crates/iac/src/registry.rs:57-115`), channel backings (`crates/dynamic/src/lib.rs:690-693`) and T1 formats (`crates/ingest/src/manifest.rs:39-53`).
- The repository has one local skill, the adapter authoring router (`.agents/skills/add-language-adapter/SKILL.md`). Nothing measures disclosure or tokens.

## 3. Decisions

ADR-0036 records decisions 1–9 (deterministic-first scope, artifacts as
projections, method versus tier, curated view, omission taxonomy and coverage
matrix, work packet, bounded investigation first, measured disclosure,
qualified test links). Four more were agreed for this roadmap:

- **Resolver upgrades need binding evidence.** A deterministic resolver upgrades a binding only with import, namespace, lexical owner, receiver type or dispatch proof. Otherwise the result is a labeled candidate or a Gap, with no new tier (#257).
- **T2 evaluation is independent.** The T2 gate needs independent holdout cases, hard negatives, per-gap-family precision, recall and abstention, and leakage controls. Retrieval and concept claims are accepted separately (#258, #260).
- **Defaults while the owner decides.** Use cases with decision tables come first, architecture and deployment views second, journeys third, with target tests in parallel. First real-provider acceptance runs on an already supported local runtime. Export and in-app views render the same projection. Existing epics stay; this roadmap maps them to phases.
- **EventBridge.** Matching uses a scoped composite identity with source attribution and distinct-identity counting. Identity migration is an implementation decision taken with owner review (#573).

### Pilot and validation lanes

The owner chose the pilot in [#575](https://github.com/true-line/cartograph/discussions/575).

- **Pilot: [SimpleLogin](https://github.com/simple-login).** Both sides are open
  source. The server, `simple-login/app`, is a Python back end and web app of
  about 1,560 files. Its clients are a browser extension in JavaScript and Vue
  and an Android app in Kotlin. The server's `docs/api.md` is a hand-written
  API reference to score the reconstructed contract against. It self-hosts
  with Docker, so the full change, re-ingest and verify cycle can run. It
  exercises cross-repo HTTP stitching (W2) and lifecycle artifacts for a real
  product domain. Known limits: `.vue` files are not parsed, Kotlin HTTP client
  calls are not recognized, the Swift clients are unsupported, and infrastructure is
  a Dockerfile and Postfix configuration with no Terraform.
- **Depth pilot: Vendure cart readiness**, unchanged. **CI fixture:** the
  synthetic multi-repo fixture, unchanged.
- **Held lanes, not scheduled:**
  - *Proton, clients only.* The server is closed. Reconstruct its contract
    from the web and Android clients, and score it against `go-proton-api` as
    a holdout. Server behavior and infrastructure stay labeled as
    interpretation. Needs HTTP client call recognition in the Go and Kotlin
    adapters and per-app ingest
    of the web clients monorepo, which exceeds the TypeScript ingest limit.
  - *MongoDB Atlas, against an official spec.* Score the contract recovered
    from the Atlas CLI, Terraform provider and Kubernetes operator against the
    published spec in `mongodb/openapi`, excluding the generated
    `atlas-sdk-go` as leakage. Needs HTTP client call recognition in the Go adapter and
    Atlas resources
    in the infrastructure registry.
  - *OpenMRS*, a Java REST server with TypeScript front-end repos, for
    enterprise scale.

## 4. Workstreams

Each workstream names its owner, what it builds on, and the observable gate
that ends it. Reviewers are the other agent unless noted.

| Id | Workstream | Owner | Builds on | Exit gate | Stories |
|----|-----------|-------|-----------|-----------|---------|
| W0 | Contracts: input and revision identity, coverage, artifact content, omission taxonomy | Claude | ADR-0036 | Contracts reviewed by both agents and referenced by W1–W6 issues | US-0033, US-0035 |
| W1 | Intake of an app or repo set by drop, with a coverage inventory before interpretation | Claude | US-0001, preflight | A dropped folder of repos ingests through the same path as the typed form and reports per-repo coverage and stopping reasons | US-0033 |
| W2 | Cross-repo stitching on binding evidence: HTTP, EventBridge, declared services | Claude | `crates/events`, #573, #257 | The synthetic multi-repo fixture stitches client-to-service calls with evidence, and unresolved calls stay Gaps with reasons | US-0034 |
| W3 | Lifecycle artifact projections with content acceptance | Claude | `crates/spec`, H2 | One complete feature (Vendure cart readiness) passes content acceptance against its independent expected set, with explicitly partial artifacts where evidence is missing | US-0035 |
| W4 | Flow and infrastructure visualization from the same projection | Claude | Atlas, Mermaid emitters | Topology, flow and deployment views render in-app and link each node to its evidence | US-0036 |
| W5 | Curated projection (H3): one revision-bound curated identity for UI, export and agents | Claude (Codex reviews the agent seam) | US-0031, SPEC-09 | Curated reads return the DR-4 fields, and stale or conflicting curation is shown, never silently substituted | — (H3, existing) |
| W6 | Work packets (AP-1a) | Codex | #255, staging review | A product-generated packet for one change reopens with the same identity, supports conflict-safe review, and renders equivalently in UI and export | US-0037 |
| W7 | Compact context and bounded investigation (AP-2a, AP-2b) | Codex; deterministic discovery is Claude's | US-0020, US-0032, #250, #256 | On a supported local runtime: discovery, exact source read, cited finding, auditor follow-up, reopened history, and an honest insufficient-evidence outcome; full MT-H4-02 obligations preserved | US-0020, US-0032 additions |
| W8 | MCP context and tasks, ACP runtime on the same coordinator | Codex | W7 accepted | H5 cross-ingress procedures executed | — (H5, existing) |
| W9 | Coverage matrix, data-file registries, plugin coverage claims, measured skills | Claude | ADR-0017, preflight | Preflight reports a language × framework × capability matrix with conformance fixtures, and each omission names its class and the seam that would fill it | US-0038 |

Shared interfaces that need both agents' acknowledgement: context revision and
provenance, the curated projection, artifact schemas, the coverage model,
target test recovery and skills.

## 5. Phases and demos

| Phase | Content | Demo that ends it |
|-------|---------|-------------------|
| P0 | W0 contracts | Reviewers can point at one schema per artifact and one omission class per Gap |
| P1 | W1, W2 on the synthetic multi-repo fixture; W3 on Vendure cart readiness; W9 matrix | Drop the fixture, see stitched calls and a coverage matrix; open the cart-readiness use case with its decision table and partial markers |
| P2 | W5 curated projection; W4 views | Accept a proposal, see it in the Workbench, the export and an agent read with the same curated revision |
| P3 | W6 packets; W7 compact investigation and discovery | Ask a scoped question, get a cited finding and an auditor follow-up, turn a gap into a reviewed packet |
| P4 | W8 MCP and ACP | An external agent reads the same context and runs the same bounded task |
| P5 | The north-star cycle on SimpleLogin | The full cycle in section 1, with its measurements recorded |

Phases are ordered by dependency, not as a waterfall. Recovered-only work
packets (AP-1a) and compact disclosure (AP-2a) can progress before the curated
projection lands, and target-test recovery runs in parallel throughout. #573 is
a separate correctness fix whose evidence the W2 stitching demo must include.
P1 runs on the synthetic fixture and Vendure. SimpleLogin enters as soon as
cross-repo HTTP stitching (W2) lands, and P5 runs on it.

## 6. First issues to file

Each is one objective. Owners in brackets.

1. Fix EventBridge channel identity matching (#573, filed) [Claude].
2. Record producing method separately from tier and confidence on every fact [Claude].
3. Omission taxonomy on Gap nodes and in the gap register [Claude].
4. Artifact schema version and content identity for each spec artifact [Claude].
5. Accept a dropped folder or set of folders as intake, through the existing ingest path [Claude].
6. Coverage inventory and stopping reasons per repo before interpretation is offered [Claude].
7. Stitch cross-repo HTTP calls with binding evidence from system manifest, base URL config or gateway routes [Claude].
8. Use-case projection with decision table for one Vendure feature, with content acceptance [Claude].
9. Render topology and flow Mermaid in-app with evidence links [Claude].
10. Deterministic discovery by identifier, name and text with snapshot-bound cursors (#250 slice) [Claude].
11. Versioned compact model projection with a per-invocation disclosure manifest (AP-2a, #256 slice) [Codex].
12. Generate and locally review a versioned work packet (AP-1a, #255 slice) [Codex].
13. Complete the real-provider investigation acceptance in MT-H4-02 against independent expected answers (AC-0191) [Codex].
14. Language × framework × capability coverage matrix with conformance fixtures [Claude].
15. Move the event SDK and channel backing registries to versioned data files [Claude].
16. Parse the script blocks of `.vue` files so the SimpleLogin extension's calls are recovered [Claude].

Existing issues stay and are re-scoped by these slices: #239 (JPA data model,
feeds W3), #250 and #256 (W7), #251 and #252 (after W7, per ADR-0036 §7), #255
(epic for W6–W8), #257, #258, #259, #260 (resolver and T2 work under the
decisions in section 3).

## 7. Keeping this plan honest

- A phase is done when its demo runs on a clean checkout, not when its issues close.
- Every new acceptance criterion lands with its test-map row in the same pull request.
- Discussion threads for the open questions: see the Discussions tab of the repository. Answers that change a decision land as an ADR amendment, then here.
