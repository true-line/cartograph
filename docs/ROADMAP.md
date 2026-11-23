# Roadmap — from structural recovery to lifecycle reconstruction

Status: agreed in [#572](https://github.com/true-line/cartograph/issues/572);
realigned on 2026-10-21 (section 8); next phase planned 2026-10-24 (section 9);
realigned to the owner's goal restatement on 2026-11-09 (section 10, which takes
precedence where it differs from earlier sections); v1.0.0 scope decided on
2026-11-17 (section 11, which takes precedence over section 10 on what v1.0.0
contains). Direction: [ADR-0036](adr/ADR-0036-lifecycle-reconstruction-direction.md)
and [ADR-0056](adr/ADR-0056-documentation-reconstruction-pipeline.md).
Vision elements V1–V13: [VISION.md](VISION.md). Baseline of sections 1–7: `main` at `cb0e876` (v0.15.5).

The decisions in section 3 were agreed between the two implementation agents
(Claude for core recovery and lifecycle artifacts, Codex for the agentic
layer). The roadmap built on them was
cross-reviewed in #574 and agreed in #572; the section 8 realignment was
cross-reviewed in #719 and approved by the owner there. It authorizes no implementation by itself: every item below lands
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
- A system is ingested repo by repo, then `stitch_backings` and `relink_found_adrs` run (`src-tauri/src/lib.rs`). Channels join across repos only because channel ids carry no repo namespace (`crates/events/src/lib.rs:178`).
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
- Two registries are Rust `const` tables with no data or plugin entry point: AWS capabilities (`crates/iac/src/registry.rs:57-115`) and T1 formats (`crates/ingest/src/manifest.rs:39-53`). Event SDKs and channel backings became versioned data files with an extension directory in #594 (`crates/adapters-fw/registries/`).
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
| W8a | Read-only MCP server and Cartograph skill over the deterministic surfaces: curated projection, spec artifacts, gap register, coverage matrix, discovery | Codex | W0, discovery (#592); W5 adds curated reads when it lands | An external agent answers a scoped question from Cartograph through MCP with cited spans, compact schemas and measured tokens, without provider calls or task execution | — (H5 read slice) |
| W8b | Agent team: durable coordinator and roster, ACP host with harness adapters and bridges, local-model agents, and the in-app human seat (#251, #252, #255) | Codex | W8a, W6 packets, SPEC-03 staging | A human and at least one ACP-hosted agent and one local-model agent work one gap in the app: the agent raises it, the human reviews, and a work packet goes to the other agent, with every agent output staged for review | — (H5, existing) |
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
| P4 | W8a read-only MCP and skill, then the W8b agent team | An agent reads Cartograph through MCP, then a human and ACP-hosted and local-model agents work a gap together in the app |
| P5 | The north-star cycle on SimpleLogin, first as a deterministic showcase | The deterministic showcase in section 8: SimpleLogin recovered end to end, its contract scored against `docs/api.md`, every gap named, published under `docs/showcases/`. The full section 1 cycle, with journeys and test arrangements, closes after W10 and W11 |

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

## 8. Realignment (2026-10-21)

P0, P1, W4 views and the deterministic parts of P3 have landed, except the
Vendure use case (#590). P2 has not: the H3 curated projection is still
unavailable, and the context query does not yet project accepted proposals.
The real-provider acceptance (#598) is open. The roadmap chained both P4 and
P5 behind W7 acceptance, and W7 waits on an owner-scheduled provider run. So
recent effort went into investigation proof machinery that has no user-visible
consumer yet, while there is still no MCP surface, no finished showcase, and
no recovered test arrangements or journeys. The order changes as follows.

1. **Receipts that scale come first.** Citable declarations have landed
   (#691, AC-0280: direct callables and config, const, decorator and header
   facts now mint primary receipts). The deterministic gap under every later item,
   including #590, is now scale: a 4,337-file TypeScript repo produces 319 MB
   of rule receipts against a 64 MiB ceiling and failed ingest outright
   (#722). Recovery now names those omissions instead of failing (#722);
   #725 compacts the receipt schema and makes retention scale, so large
   repos become citable rather than only recoverable.
2. **W8 is no longer gated on W7.** A read-only MCP server and a Cartograph
   skill (W8a) expose what already exists deterministically, and become the
   agent team's read path. W8a's boundary:
   - Its initial scope is recovered and qualified evidence with rejected
     proposals filtered out. Curated (H3) capabilities report as unavailable
     until W5 lands, so W5 adds curated reads to W8a rather than gating it.
   - It reuses the existing deterministic projections and retained evidence,
     and fails closed on a snapshot mismatch or an unavailable capture.
   - It exposes no task, start, ingest, write or provider tools.
   - It keeps omissions and partial results honest.
   - Token measurements name their tokenizer or method, or say "estimate"
     or "unknown".
   - It claims no H5 task or ACP acceptance; those stay with W8b.

   W8b, the agent team, no longer waits for W7 either (ADR-0036 decision 7
   as amended). The desktop app is the human seat. Agents run on local models
   or through ACP in any harness, directly or through an adapter or bridge,
   and every agent output is staged for human review. The human reaches the
   team from anywhere in the app with one shortcut (Shift+Cmd+Space): pick
   an agent, or let the app pick one for the question. The agent receives
   the current view and selection, answers with citations, and can act in
   the interface only through staged proposals. W7 acceptance now gates
   only the claim that a specialist run is accepted evidence.
3. **P5 starts as a deterministic showcase.** SimpleLogin runs end to end on
   deterministic artifacts, the recovered contract is scored against
   `docs/api.md`, every gap is named, and the result is published under
   `docs/showcases/`. Its findings set the next priorities.
4. **Acceptance harness verification can proceed.** The owner authorized
   #716's conditional-safety revision on 2026-10-27: each admitted prompt and
   its finish/refusal path must fit existing limits. Full-budget fit is no
   longer a prerequisite. Refused evidence or partial completion does not
   satisfy #598, which stays open. Real-provider execution still requires a
   separately authorized run definition; historical controls remain frozen.
5. **Recorded business flows and journeys follow the showcase.** Enterprise
   estates typically have outdated or missing documentation, no recorded
   journeys, and no recorded business flows to test against. Those are the
   artifacts this product exists to recover, so W10, test arrangements and
   validation (recovered business flows and target tests with qualified
   links), and W11, journeys, are next after the showcase rather than
   unscheduled. The showcase decides which goes first. Each also reports
   drift: where existing documentation disagrees with what the code shows,
   as the showcase does against `docs/api.md`.
6. **An explorable estate view (W12).** The user-facing goal is one
   interactive surface over a whole estate, in the spirit of a live globe of
   public signals but over software: move from the estate to a system,
   service, flow, rule and source line; select a flow and follow it across
   repos and infrastructure; switch layers for infrastructure roles, flows,
   journeys, rules, tests, ownership, gaps and documentation drift; ask the
   agent team about what is in view; and share a link that restores the
   view and selection. Every layer shows its evidence tier and says when it
   is partial, stale or unavailable, and each layer is a module that a plugin
   can add. Today's Atlas falls short of this: its Infrastructure layer shows
   rule gaps rather than what the code runs on or talks to (#723).
   W12 builds on W4 views, the W8a read surface and the W8b human
   seat. It is planned after the showcase, with scale tested on the larger
   lanes.
7. **v2 horizon: targets without source.** After v1 works on source,
   Cartograph should take a website, mobile app, desktop app or similar
   artifact, such as an old site of the owner's, and break it down the way
   REA-style reverse engineering does: page structure, bundles, network
   observations, binaries and runtime behavior. This observation layer sits on
   top of v1 and fills its missing pieces. It never replaces it. Business
   rules, journeys and infrastructure roles can be confirmed only from source,
   so observed evidence stays a separately labeled kind that can support or
   contradict a claim but cannot confirm a rule on its own. To keep v2 cheap
   to add: keep source and observation provenance distinct in the tier model,
   keep omission fill seams able to route a gap to an observation source, and
   prefer composition over rebuilding. W8a's MCP surface lets an agent use an
   observation tool such as REA alongside Cartograph, and observed facts could
   enter through a manifest or plugin. Unscheduled until v1's showcase lands.

Identifiers reserved for this realignment: US-0041 and AC-0288..0290 for
W8a, plus ADR-0044 (Codex). AC-0291 and ADR-0043, first reserved for #691,
are retired unused because #691 landed under AC-0280. AC-0292 is #722, and
ADR-0045, reserved for the living-wiki design (#724), is written for #1749.
The next free identifiers are US-0042, AC-0293 and ADR-0046.


## 9. Next: test arrangements, then journeys (2026-10-24)

The SimpleLogin showcase ([`docs/showcases/simplelogin.md`](showcases/simplelogin.md))
has landed. Its contract scores 44 of 45 documented endpoints, the clients
link to 43 server endpoints, and the data model, schema drift, observed
infrastructure, scheduled flows and API documentation drift are all
recovered. Section 8 item 5 left the choice between W10 and W11 to the
showcase. The showcase points to **W10 first**:

- SimpleLogin ships 161 test files that make about 425 Flask test-client
  calls. Most address their endpoint by name (`url_for("api.get_aliases")`)
  and assert a status code or response key. That evidence is deterministic,
  and it links directly to the endpoints, the Blueprints and the responses
  (#940) that are already recovered.
- Cartograph recovers no target tests today, so test arrangements are the
  largest missing lifecycle artifact that source can fully prove.
- Journeys need client navigation facts that no adapter produces yet.
  A journey is also a rule-derived interpretation (ADR-0036 decision 3),
  so it should build on flows that tests already qualify.

| Id | Slice | Owner | Exit gate |
|----|-------|-------|-----------|
| W10a | Python target tests as static references: pytest test functions, fixtures and `conftest.py` scope; each Flask test-client call resolved to a recovered `Endpoint` by endpoint name (Blueprint name and handler) or by literal path through the existing route matcher; asserted status codes and response keys recorded as declared expectations; an unresolved or dynamic call, and fixture setup that is not read, stays a Gap naming the call | Claude | On SimpleLogin, `test_arrangements.md` lists for each endpoint the tests that reference it and what they assert. An endpoint with no recovered reference is a named omission. That omission is not proof that no test exists, and a reference is not proof that a test ran or which branches it reaches. Content acceptance uses an independently hand-labelled sample of tests and endpoints, not counts |
| W10b | Test and contract drift: an asserted status or key that the recovered responses lack, and a recovered status that no test asserts, compared only where the endpoint and response scope are the same | Claude | `drift_register.md` reports both directions on SimpleLogin, cited on both sides, and an endpoint whose responses or test references are partial is reported as incomplete evidence, not as drift |
| W10c | A second test framework as its own module, behind the W9 coverage matrix's target-test capability | Claude | Vendure's end-to-end suite links its operations to recovered resolvers, which gives #590's 41 unexecuted decision cases their test evidence or a named omission |
| W10d | Flows qualified by test references: each flow dossier names the tests that statically reference its participants and the steps no recovered test references | Claude | SimpleLogin's 18 scheduled flows and its client-to-server flows show test references per step, never claimed as executed coverage |
| W11 | Journeys: deterministic client navigation facts first (Android activities and Compose navigation, the extension's popup routes), then journey candidates over those facts, client calls (#737), server flows and their tests | Claude, design reviewed by Codex | One SimpleLogin journey, such as creating an alias from the extension, scored against an expected set written from SimpleLogin's public help before the run. Journey candidates are T2 rule-derived and InferredWeak, never T0 |

The agentic side runs alongside this, and each part is Codex's to scope:

- **#937 first.** It makes finish refusals diagnosable. Bounded correction
  or retry is qualified separately, and the fix does not promise that a
  model passes, reads source or recovers. #766's unfinished delivery
  remains an existing obligation alongside the lanes below.
- **W8a.** The read surface gains test arrangements once W10a's producer
  contract is stable: bounded, revision-pinned reads with provenance and
  explicit unknown and omission semantics.
- **W6.** A product-generated work packet proposes a task from an
  endpoint with no recovered test reference, or from a test and contract
  drift entry. It carries the endpoint, its cited handler, its recovered
  responses, the missing assertion and the evidence's limitations. A packet
  is a proposal: it does not prove a missing implementation or authorize
  its execution. This is the first end-to-end use of the north-star
  cycle's "hand the gap to an implementer".

W12, the explorable estate view, gains a tests layer from W10 and a journeys
layer from W11. It stays planned after both.

Identifiers: US-0044 and AC-0343 onward for W10. W11 takes the next story
and an ADR for journey identity and tier, which Claude drafts before W11
starts. ADR-0049 stays reserved for the shared extraction-cache draft under
review, so the journey ADR takes the next number unused on `main` when it
is drafted. That is ADR-0050, drafted on 2026-10-25.

## 10. Realignment (2026-11-09)

The owner restated the goal on 2026-11-09 (#1745,
[ADR-0056](adr/ADR-0056-documentation-reconstruction-pipeline.md), VISION
V10–V13). It is a pipeline of five stages. An audit of `main` at `d36e7e08`
(v0.67.1) measured the code against each stage. This section records the
result and reorders the work. Like the rest of this roadmap it authorizes no
implementation: every item lands through its own issue.

### Where each stage stands

| Stage | On `main` today | Missing | Issues |
|-------|-----------------|---------|--------|
| 1. Gather an app, or as much of its code as exists | Registered sources (local, `file://`, GitHub), system manifests, partial ingestion with a coverage inventory | The coverage inventory is lost on relaunch | #1655, #1656 |
| 2. Trace it deterministically: infrastructure, code, UI/UX, documentation | T0 adapters for six languages, Terraform and AWS, events and flows; twenty-six compiled artifacts (`ARTIFACTS` in `crates/spec/src/workbench/selected.rs`); 44 of 45 SimpleLogin endpoints; a 61-repository OpenMRS run | Documentation is read only as API descriptions and found ADRs; the knowledge phase skips graphs over 50,000 rows; the graph content hash exists only in tests | #1750, #1771, #1765, #1658 |
| 3. The agent team writes the document set | Bounded specialists answer cited questions and stage proposals. An export lists accepted proposals with their state in `curated.md` (SPEC-12 slice 2); nothing they produce reaches any other document, the Workbench does not show curated items, and the curated projection is unavailable to MCP readers (`src-tauri/src/mcp_reads.rs:169`) | The curated projection, the document catalogue, section drafting | #1746, #1747, #1748, #1751 |
| 4. The documents follow the code | Drift is detected and listed in `drift_register.md` | Sections that know their facts, stale marking, redrafting | #1749 |
| 5. Engineers rebuild or port (enabled, not performed) | Local work packets whose only state is `Unoffered` (`crates/work-packets/src/store.rs:14-16`); the export bundle | A defined handoff bundle and a sufficiency check | #1752 |

Stages 1 and 2 are the strong part of the product. Stage 3 exists as
question answering, which serves the context hub mode, and not yet as
authorship. Stages 4 and 5 have their inputs and no product path.

### New workstreams

| Id | Workstream | Issue | Exit gate |
|----|------------|-------|-----------|
| W5 | Curated projection (unchanged from section 4; now first) | #1746 | An accepted proposal appears in the Workbench, a `best-effort` export and an MCP read under one curated revision; the base graph hash is unchanged |
| W14 | Enterprise document catalogue | #1747 | A versioned catalogue approved by the owner: each document's reader, sections, deterministic inputs, authorable sections and acceptance |
| W13 | Agent-authored document sections | #1748; contract proposed in [SPEC-15](SPEC-15_authored-sections.md), ADR-0064 | One SimpleLogin document with authored sections scored claim by claim against an expected set written before the run |
| W15 | Living documentation | #1749; contract proposed in [SPEC-17](SPEC-17_living-documentation.md), ADR-0045 | Changing one handler between two commits marks exactly the sections that cite it as stale, the same way on every run |
| W16 | Existing documentation as evidence | #1750; contract proposed in [SPEC-18](SPEC-18_documentation-evidence.md), ADR-0065 | A README reference that resolves is linked at T0; one that no longer resolves appears in the drift register with both sides cited |
| W17 | Rebuild handoff bundle | #1752; defined in [SPEC-16](SPEC-16_rebuild-handoff-bundle.md) | One SimpleLogin bundle reviewed against the sufficiency checklist by someone who did not produce it |

### Order

1. **W5 first.** It was P2 in section 5 and never started. Every later
   stage needs a place where an accepted item becomes visible.
2. **W14 next**, in parallel with W5. It is a decision, not code, and W13
   needs it to know what to write.
3. **W13**, on W5 and W14.
4. **W15**, on W13. ADR-0045 is written first.
5. **W16** runs alongside from the start; it depends only on the ingest
   path.
6. **W17** follows W14 and is revised once W13 produces documents.

W10 to W12 in section 9 continue: test arrangements and journeys are
deterministic inputs to the document set.

### The two modes

Investigations, the specialist roster, the MCP read surface and the
context query are the **context hub** mode. They stay, and their
acceptance (#598, #1710) gates that mode's claim. They are not the
route to stage 3: section drafting reuses their admission, citation and
refusal machinery with a different finish payload. The specialist roster
is consolidated before it gains a drafting role (#1751).

**Reconstruction** mode is the pipeline above. Its claim is gated by
content acceptance of authored documents (W13), not by question answering.

### What the v1.0 criteria prove

| v1.0 criterion (#1610) | Stage or mode it proves |
|------------------------|-------------------------|
| Large-estate run recorded | Stages 1 and 2 at scale |
| SimpleLogin pilot on a release build | Stages 1 and 2 on a real application, and the context hub mode end to end |
| Frozen rubric and real-model acceptance (#598, #1710) | Context hub mode |
| Readiness report | All of the above |

None of these criteria proves stage 3 as authorship, or stage 4. The owner
decided on 2026-11-17 that v1.0.0 carries reconstruction criteria as well:
see [section 11](#11-v100-scope-2026-11-17).

### Defects that bear on the pipeline

Found by the same audit and filed separately. Integrity and privacy:
#1753 (redaction misses compound secret names), #1754 (MCP evidence text
is not redacted), #1755 (topology export carries no tier), #1756 (plugin
provenance is not validated), #1767 (egress record omits failed calls).
Verification and delivery: #1758, #1759, #1768, #1770, #1762, #1764,
#1769. Setup and records: #1763, #1757, #1761, #1760.

### Milestones and epics

Every open issue was assigned to one of these GitHub milestones on
2026-11-09. They are worked in this order; each milestone's description
holds the order of the issues inside it. M0–M10 remain the closed SPEC-00
milestones.

| Milestone | Exit | Holds |
|-----------|------|-------|
| R1 — Foundation and context hub (release candidate) | The foundation and context hub criteria of #1610 pass on a release build. No v1.0.0 tag: see section 11 | Release-blocking defects, the acceptance track (#598, #1710), pilot findings, the readiness report |
| R2 — Curated projection and document catalogue | An accepted proposal is visible in the Workbench, the export and an MCP read under one curated revision; the catalogue is approved | W5, W14, #1751, #1761, #1765 |
| R3 — Agent-authored documents | One SimpleLogin document with authored sections scored against an independent expected set | W13, W16, the knowledge and semantic layer, W10 and W11 inputs |
| R4 — Living documentation and rebuild handoff; v1.0 GA | Stale marking is exact and repeatable on a fixture; one handoff bundle passes its review | W15, W17 |
| Engineering health | Continuous | CI, tests, release mechanics, internal scale |
| Later | Not scheduled | Experiments and platform work off the path |

Three epics group work that crosses milestones, with the work as their
sub-issues: #1773 (the pipeline: W5, W13 to W15, W17), #1774 (trace
completeness for the document set) and #1775 (integrity and privacy
hardening).

## 11. v1.0.0 scope (2026-11-17)

Section 10 left one decision to the owner: whether v1.0.0 carries a
reconstruction criterion, or ships as the deterministic foundation with the
context hub. The owner decided on 2026-11-17 (#1853): **v1.0.0 is the full
pipeline of [ADR-0056](adr/ADR-0056-documentation-reconstruction-pipeline.md)**,
all five stages and both modes. Like the rest of this roadmap, this section
authorizes no implementation: every item lands through its own issue.

Rebuild stays enabled, not performed (NG2). An enterprise user takes
Cartograph's documents and evidence as the source for rebuilding or porting
the application. Cartograph writes no target code.

### v1.0.0 criteria

The criteria are the existing ones on #1610 plus the exit gates that section
10 already set for the pipeline workstreams. Nothing here is a new gate.

| # | Criterion | Proves | Source |
|---|-----------|--------|--------|
| 1 | Large-estate run recorded with time, memory and gaps | Stages 1 and 2 at scale | #1610 |
| 2 | SimpleLogin pilot passes on a release build | Stages 1 and 2 on a real application; context hub end to end | #1610 |
| 3 | Frozen rubric and a real-model acceptance run | Context hub mode | #598, #1710 |
| 4 | An accepted proposal appears in the Workbench, a `best-effort` export and an MCP read under one curated revision; the base graph hash is unchanged | Stage 3 has a place to land | W5, #1746 |
| 5 | A versioned document catalogue approved by the owner: each document's reader, sections, deterministic inputs, authorable sections and acceptance. Met by catalogue version 1.0, approved 2026-11-17 | Stage 3 knows what to write | W14, #1747, #1856 |
| 6 | One SimpleLogin document with authored sections, scored claim by claim against an expected set written before the run | Stage 3 as authorship (reconstruction mode) | W13, #1748 |
| 7 | Changing one handler between two commits marks exactly the sections that cite it as stale, the same way on every run | Stage 4 | W15, #1749 |
| 8 | A README reference that resolves is linked at T0; one that no longer resolves appears in the drift register with both sides cited | Stage 2 reads existing documentation as evidence | W16, #1750 |
| 9 | One SimpleLogin handoff bundle reviewed against the sufficiency checklist by someone who did not produce it | Stage 5, enabled | W17, #1752 |
| 10 | Owner-approved readiness report covering 1 to 9 | All of the above | #1610 |
| 11 | v1.0.0 tagged | The release decision | #1610 |

### What changes in section 10

- **The tag moves.** v1.0.0 is tagged when R4 exits, not R1. R1 to R4 are
  the four stages of one release. R1's exit is its criteria passing on a
  release build, which makes that build a release candidate for the
  foundation and context hub. The milestone table in section 10 names R1
  and R4 accordingly. The GitHub milestone still titled "R1 — v1.0 GA:
  foundation and context hub" is renamed to match, and R4 becomes the v1.0
  GA milestone; until the owner renames them, this section governs.
- **The order does not change.** W5 and W14 first and in parallel, then W13,
  then W15 and W17, with W16 alongside. The integrity and privacy defects of
  #1775 that bear on a release build stay release-blocking.
- **Providers.** Reconstruction runs on local open-source models and, where
  the user opts in with their own key, on cloud providers. In-app provider
  setup is #1763 (ADR-0057). How the agent team runs on those providers is
  proposed in ADR-0066 (owner review): a role runs in the app on a local
  model, the Claude Agent SDK or the OpenAI Agents SDK, the app hosts the
  SDK runtimes with the user's key, every output passes the same
  admission and review, and outside agents such as Codex and Claude Code
  work with the in-app agents through MCP.

### Tracking

Every open issue carries the organization **Stage** issue field, set from its
milestone on 2026-11-17, so the planning board
([project 5](https://github.com/orgs/true-line/projects/5)) can order the
work. The milestone stays the source: a new issue gets the Stage of its
milestone.

| Milestone | Stage |
|-----------|-------|
| R1 | 1 |
| R2 | 2a |
| R3 | 3 |
| R4 | 4 |
| Engineering health | Maintenance |
| Later | Post-MVP |
