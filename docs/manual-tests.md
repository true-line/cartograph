# Manual test procedures

Milestone-boundary verification a human performs by using the app — the half
of an exit gate that automation cannot see (window chrome, feel, end-to-end
reality). Each procedure has a stable id referenced from
[`test-map.md`](test-map.md); CI verifies the reference, a human performs the
steps. Record results in the closing comment of the milestone's task issue.

Convention: run the relevant procedures at each milestone boundary, not per
PR — per-PR verification is CI's job.

---

## MT-M0-01 — Shell boots, job spine survives restart

1. `npm run tauri dev` from the repo root.
2. Window opens; dark theme; badge reads **core vX.Y.Z** (green).
3. Click **Enqueue test job** → a `noop / queued` row appears.
4. Quit the app fully; relaunch.
5. **Pass:** the job row is still listed (durable spine, M0 exit gate).

## MT-M1-01 — Ingest a TS repo, walk endpoint → evidence

1. `npm run tauri dev`; paste the path of a TypeScript/Express repo into
   **Ingest** and submit.
2. Job runs to `done`; graph stats become non-zero; **Endpoints** lists
   recovered routes, each with a **Confirmed** tier badge.
3. Click an endpoint.
4. **Pass:** the evidence panel shows tier/extractor/`repo:path bytes@commit`
   and the read-only source with the registration call highlighted — the
   highlighted text is the actual registration in the actual file
   (M1 exit gate: evidence jump-to-source).

## MT-M2-01 — Ingest Terraform, export the topology map

1. `npm run tauri dev`; paste the path of a repo containing `.tf` files into
   **Ingest** and submit.
2. Graph stats grow; the **Topology map** card shows Mermaid text with
   `Resource` nodes and solid `TRIGGERS`/`ROUTES`/`SUBSCRIBES`/`GRANTS` edges
   where the Capability Registry matched (dotted for the reference DAG).
3. Click **Copy Mermaid**, paste into a Mermaid renderer (e.g.
   mermaid.live).
4. **Pass:** the rendered diagram matches the repo's infrastructure; anything
   the extractor could not resolve appears as a visibly distinct `?` node,
   never silently dropped (M2 exit gate: topology map artifact).

## MT-M3-01 — Trace flows, export the dossier

1. `npm run tauri dev`; ingest a TypeScript repo with Express endpoints and
   event SDK usage (emitter/Kafka/SQS — any repo exercising US-0004).
2. The **Flows** card lists each traced flow with a status and score; any
   runtime-computed channel appears as a `GAP: …` hop with a reason, and
   its branch stops there — never silently completed (R-INT-4).
3. Click **Copy dossier**, paste into a Markdown+Mermaid renderer.
4. **Pass:** each flow renders a sequence diagram (Gap arrows broken `--x`)
   and a provenance table with tier + confidence + evidence span on every
   hop (M3 exit gate: flow dossier export).

## MT-M4-01 — Screen-anchored flows

1. `npm run tauri dev`; ingest a repo with a React client (React Router or
   Next.js `pages/`) fetching its own backend's endpoints.
2. The **Flows** card anchors flows at screens (`Screen /route`), not at
   the endpoints those screens fetch; endpoints nothing fetches keep their
   own flows.
3. Copy the dossier and render it.
4. **Pass:** a screen flow runs `RENDERS → FETCHES → HANDLES → …` end to
   end with tier + confidence per hop; an unresolvable fetch URL appears
   as a `GAP: …` hop truncating that branch (M4 exit gate: flows anchored
   at Screen).

## MT-M5-01 — Cross-repo flow via the system manifest

1. Write a `cartograph.system.toml` declaring 2+ repos that share a channel
   (a queue URL / topic in both, or via the manifest `[env]` block); paste
   its path into **Ingest** (fresh graph — #50 clears stale schemes
   automatically).
2. The summary lists every repo as `identity@sha12`.
3. The **Flows** card shows one flow whose trigger lives in one repo and
   whose consumer hops land in another — inspect the dossier: the
   `SUBSCRIBES` hop's target carries the other repo's identity.
4. **Pass:** the cross-repo flow is Verified when both sides resolve
   (literal or manifest identity); an unresolved side appears as a `GAP: …`
   hop, never a silent stitch (M5 exit gate: cross-repo flow via literal
   channel ids).

## MT-M6-01 — Observed state joins infra to code (T1)

1. In a repo with Terraform + publishing code, run
   `terraform show -json > state.json` (or `terraform show -json plan.out`
   for a plan); add `state_json = "state.json"` to that repo's `[[repos]]`
   entry in `cartograph.system.toml` and ingest the manifest.
2. The **Topology map** shows the backed channel as a cylinder with a
   `BACKS` arrow from its resource — infra and the event layer are one
   picture.
3. Inspect an enriched resource's evidence: T0 `prov` (Deterministic)
   remains, and `observed_prov` (Dynamic, Confirmed) points into the state
   file; any placeholder the state confirms has lost its `?`.
4. **Pass:** BACKS appears only for channels code actually publishes or
   subscribes; values `terraform show` marks sensitive read `[redacted]`
   everywhere in the UI (M6: observed-fact provenance; AC-0009).

## MT-M6-02 — OTLP trace fills a runtime channel Gap (T1)

1. In a repo whose event SDK call computes its queue/topic identity at
   runtime, capture an OTLP trace with `messaging.system`,
   `messaging.destination.name`, and `code.file.path` span attributes.
   Export it with the collector file exporter as OTLP/JSON Lines.
2. Add `otel_jsonl = ["trace.jsonl"]` to that repo's `[[repos]]` entry in
   `cartograph.system.toml` and ingest the manifest.
3. Inspect the previously unresolved PUBLISHES/SUBSCRIBES hop: its Gap is
   replaced by a Channel whose edge resolver is `t1.otel-trace`; provenance
   is Dynamic/Confirmed and points to the observed span id in `trace.jsonl`.
4. Include `http.request.method` plus `http.route` on an HTTP server span.
   The matching Endpoint keeps its Deterministic `prov` and gains separate
   Dynamic `observed`/`observed_prov` facts.
5. **Pass:** a uniquely source-matched identity resolves the Gap; ambiguous
   same-kind observations leave the Gap explicit with T0 and T1 recorded in
   `attempted_tiers` (AC-0012, R-INT-1, R-INT-4, M6 exit gate).

## MT-M6-03 — Pulumi program and observed deployment form a T0→T1 ladder

1. In a TypeScript Pulumi AWS repo, run `pulumi stack export --file stack.json`
   without `--show-secrets`; add `pulumi_json = "stack.json"` to that repo's
   `[[repos]]` entry in `cartograph.system.toml`, then ingest the manifest.
2. Inspect an import-proven resource created with `new aws.*`: its T0 `prov`
   is Deterministic and points to the constructor, while REFERENCES,
   `dependsOn`, `parent`, and Capability Registry edges remain T0.
3. Inspect the same resource's `observed` and `observed_prov`: the observed
   URN/inputs/outputs come from the stack artifact with Dynamic/Confirmed
   evidence, without replacing the T0 fact.
4. Repeat with `pulumi preview --json` output. Include an encrypted Pulumi
   secret wrapper in the fixture or stack and verify Cartograph displays
   `[redacted]`, never ciphertext or plaintext.
5. **Pass:** only observations matching an existing T0 Pulumi type + logical
   name enrich the graph; an unmatched exported resource does not create a
   new T0 Resource (AC-0051, AC-0052, R-INT-1).

## MT-M7-01 — Local semantic resolution clears its precision gate (T2)

1. Start Ollama locally and make the configured embedding model available:
   `ollama pull nomic-embed-text` (Cartograph never downloads it implicitly).
2. Run
   `cargo test -p semantic real_ollama_resolves_eval_gated_gap -- --ignored --nocapture`.
   Also run
   `cargo test -p app semantic_preview_uses_real_ingested_resource_and_call_gaps`;
   this fixture must recover its inputs through the production TypeScript,
   event, and Terraform extractors rather than constructing graph nodes by hand.
3. Inspect the printed report: provider is local Ollama, paired-eval precision
   meets the configured floor, ANN lookup is below 100ms, and one explicit
   channel Gap is replaced only in the returned best-effort overlay.
4. Stop Ollama and repeat the semantic preview from the app/API.
5. **Pass:** the stopped provider fails explicitly with no graph change or
   network fallback; the passing preview edge is Semantic/InferredStrong with
   evidence from both Gap and target, while the stored confirmed graph retains
   its original Gap. The real-ingest fixture fills both the IaC-backed channel
   Gap and unresolved relative-import call Gap without adding gaps for globals
   or package calls (AC-0021, AC-0022, R-INT-1, M7 exit gate).

## MT-M8-01 — Bounded T3, exact egress consent, and durable curation

1. Run `cargo test -p llm -p agents`. Confirm the local-only cloud test
   reports zero provider calls, and the bounded broker rejects Confirmed slots,
   invented targets, and missing both-side citations before staging anything.
2. Run `npm --prefix ui run storybook` and open
   **Privacy / EgressConsentDialog / ExactSpanPayload**.
3. Compare every displayed field with the story's firewall preview fixture:
   provider, tier, one-action id, system instructions, prompt, both repo/path/
   byte/commit spans, redacted span text, redaction count, and payload hash.
   Resize below 600 px and confirm no payload text is clipped or hidden.
4. Click **Allow this action once**. **Pass:** the interaction fires only the
   consent callback with that complete preview; no unredacted secret appears.
5. Run
   `cargo test -p agents accepted_and_rejected_decisions_persist_and_reapply_by_basis`.
   **Pass:** the final accept/reject state survives SQLite reopen and reappears
   for the unchanged task basis, while changed evidence has no inherited
   decision.
6. With Ollama and `qwen3:8b` already installed locally, run
   `cargo test -p agents real_ollama_returns_bounded_cited_agent_proposal -- --ignored --nocapture`.
   Stop Ollama and repeat. **Pass:** local failure
   is explicit and no cloud provider is selected; Cartograph never pulls a
   model automatically (AC-0020, AC-0023..0025, R-INT-1, R-INT-3, M8 exit gate).

## MT-M9-01 — Atlas filters, confidence integrity, and 10k-node interaction

1. Run `npm --prefix ui run storybook` and open
   **Atlas / AtlasCanvas / TenThousandNodeScale**. Pan and zoom the 10,000-node
   Cytoscape canvas, then switch through Infrastructure, Cloud, Server, Events,
   and Client.
2. **Pass:** controls remain responsive and each filter reports only its own
   node/edge projection; the app does not create 10,000 parallel DOM controls
   (the accessible entity index stays bounded).
3. Open **Atlas / AtlasCanvas / ConfidenceOverlay** and compare the legend to
   the canvas. **Pass:** Confirmed is green, InferredStrong blue,
   InferredWeak yellow, and Gap red with a dashed diamond; disabling the
   overlay removes tier color without relabeling facts.
4. Open **Shell / App / AtlasNodeToEvidence**, select the endpoint from the
   Atlas entity index, and inspect the evidence drawer.
5. **Pass:** file, byte span, commit, extractor, and tier are visible; the
   matching source span is highlighted in a read-only view (AC-0026..0028,
   R-INT-2, NG1).

## MT-M9-02 — Flow Inspector sequence, explicit Gap, and export projection

1. Run `npm --prefix ui run storybook` and open
   **Atlas / FlowInspector / SequenceAndTriggerSelection**. Select each trigger,
   then pan, zoom, and fit the React Flow viewport.
2. **Pass:** the visual and accessible sequences follow the traced hop order;
   every hop carries a distinct tier/confidence badge, and the selected source
   flow's status and score remain visible (AC-0029, R-INT-2).
3. Open **Atlas / FlowInspector / ExplicitGap**. **Pass:** the unresolved hop is
   a dashed red card that shows the graph-provided reason and attempted tier
   sequence; no downstream hop is invented after the Gap (AC-0030, R-INT-4).
4. Open **Atlas / FlowInspector / VerifiedOnlyProjection**, switch from
   `best-effort` to `verified-only`, and expand the Mermaid + provenance dossier.
5. **Pass:** the InferredWeak hop disappears from both the visible sequence and
   copyable dossier, while Confirmed and explicit Gap hops remain annotated
   (AC-0031, R-INT-5).

## MT-M9-03 — Spec Workbench provenance, curation, and full export

1. Run `npm --prefix ui run storybook` and open
   **Spec / SpecWorkbench / FullArtifactSetAndInlineProvenance**.
2. Select each of the nine artifact entries. **Pass:** user stories, US-TM,
   flow dossiers, resource topology, data model, ADRs, Gap register, and Drift
   register plus Security findings are always present; every recovered assertion shows its tier,
   confidence, extractor, content hash, and every evidence span inline
   (AC-0032, AC-0035, R-INT-2).
3. Open **Spec / SpecWorkbench / AcceptRejectAndAnnotate** and exercise all
   three curation controls on the inferred assertion. **Pass:** only inferred
   content exposes the controls, annotation requires a note, and the resulting
   decision appears in the durable curation log without changing the tier
   badge (AC-0033, R-INT-1).
4. Open **Spec / SpecWorkbench / VerifiedOnlyExport**, switch between both
   modes, and use **Export bundle**. **Pass:** `verified-only` excludes weak
   inference, `best-effort` clearly tags it, and both exported projections
   contain the Gap and Drift registers (AC-0034, R-INT-5).
5. In a connected desktop build, record a decision, re-ingest the unchanged
   source, and reopen the Workbench. **Pass:** the decision reappears for the
   same content hash; changing its source/evidence produces a new undecided
   assertion (AC-0033).

## MT-M9-04 — Found/recovered ADRs and mapped drift

1. In a multi-repo fixture system, add `docs/adr/ADR-0001.md` to a docs repo
   with `Status`, `Governs`, and `Forbids` fields. Make `Governs` cite an
   existing graph id from a service repo in backticks and create a code edge
   whose label is listed by `Forbids`.
2. Ingest the system and open the Workbench **Architecture decisions** artifact.
   **Pass:** the found ADR and DECIDES link are Confirmed with exact file/span
   evidence; unrelated or nonexistent ids are not linked (AC-0036).
   Remove the `Governs` declaration and re-ingest. **Pass:** its former DECIDES
   link is absent. Delete the ADR file and re-ingest. **Pass:** its found ADR
   node is absent (AC-0036).
3. Include a code producer and channel not governed by a found ADR. **Pass:**
   the artifact includes a distinct **Recovered / Inferred** ADR with graph
   evidence and curation controls; it is never displayed as Confirmed
   (AC-0037, R-INT-2).
4. Open **Drift register**. **Pass:** the found-ADR conflict names the ADR,
   offending edge, any containing flow trigger, and confidence inherited from
   the offending fact. Reject the supporting inferred edge and export again.
   **Pass:** neither its recovered ADR nor its drift finding remains
   (AC-0037, AC-0038).

## MT-M9-05 — Explicit endpoint auth and IAM security findings

1. Ingest a fixture with three endpoint facts: one explicitly
   `authenticated: false`, one explicitly protected, and one with no recovered
   auth state. Include IAM `GRANTS` with both least-privilege actions and a
   wildcard action or literal wildcard resource scope.
2. Open Workbench **Security findings**. **Pass:** only the explicit negative
   endpoint appears as unauthenticated; the protected and unknown-auth
   endpoints do not. The row cites its evidence and maps to US-0015/AC-0041.
3. **Pass:** the wildcard grant appears with its exact action and resource
   scope, confidence, evidence, and US-0015/AC-0042 mapping. The bounded grant
   does not appear.
4. If the wildcard `GRANTS` support is inferred, reject it and export again.
   **Pass:** its derived finding disappears; confirmed findings and facts are
   unchanged (R-INT-1, R-INT-5).

## MT-M10-01 — Deterministic delta re-ingest

1. Ingest a local fixture containing at least two TS/TSX files and two
   Terraform files. Record the returned delta counts and the Atlas snapshot.
   **Pass:** every source context is initially reported recomputed.
2. Re-ingest without changing any input. **Pass:** recomputed is zero, source
   contexts are reported reused, the ordered T0 fact identity/content-hash set
   is identical, and graph reconciliation reports no changed facts (AC-0039).
3. Change one TS/TSX file and one Terraform file, then re-ingest. **Pass:** only
   those byte-changed extraction contexts are reparsed; unchanged contexts are
   reused, while cross-file calls, module/policy joins, and stitched facts
   reflect the new full repository state (AC-0040).
4. Delete one changed source and re-ingest. **Pass:** its cache context and
   graph facts disappear; no stale node/edge remains (AC-0040).

## MT-M10-02 — Python server recovery and language summary

1. Ingest a Python repo containing both an import-proven FastAPI route and an
   import-proven Flask route, with one handler calling a function imported
   from another local Python module.
2. **Pass:** the ingest summary reports Python file/node/edge counts separately
   from TypeScript and Terraform; zero-count languages remain visible.
3. Inspect both endpoints and their handlers. **Pass:** methods, literal paths,
   HANDLES, local/imported CALLS, tier, extractor, file, exact byte span, and
   commit are present and Confirmed.
4. Add a lookalike object exposing `.get`/`.route` without a FastAPI/Flask
   import. **Pass:** it creates no Endpoint. Re-ingest unchanged, then change
   one Python file. **Pass:** unchanged Python contexts are reused and only the
   changed file is recomputed (AC-0053, ADR-0003, M10 language breadth).

## MT-M10-03 — Go server recovery and language summary

1. Ingest a Go module containing import-proven `net/http`, chi, and gin route
   registrations, with one handler calling a function in another local package.
2. **Pass:** the ingest summary reports Go file/node/edge counts separately
   from TypeScript, Python, and Terraform; zero-count languages remain visible.
3. Inspect the endpoints and their handlers. **Pass:** methods, literal paths,
   HANDLES, local/imported CALLS, tier, extractor, file, exact byte span, and
   commit are present and Confirmed. A route whose handler is a computed or
   external expression remains present with an explicit HANDLES Gap.
4. Add a lookalike router without a matching import and a computed route.
   **Pass:** neither creates an Endpoint. Mark the repo client-only and ingest;
   **pass:** no Go facts are produced. Re-ingest unchanged, then change one Go
   file. **Pass:** unchanged Go contexts are reused and only the changed file is
   recomputed. Add a `//go:build ignore` file and a GOOS-suffixed file without
   declaring a build target. **Pass:** neither contributes Confirmed facts
   (AC-0054, ADR-0003, M10 language breadth).

## MT-M10-04 — Local scale benchmark (AC-0423, T-0423)

**Procedure: local only, never CI.** It reads pinned public repositories and
never runs their code. Timing and memory are measurements, not gates.

1. On macOS or Linux with `git`, `sqlite3` and the Rust toolchain, run
   `node scripts/bench-ingest.mjs --work <new dir>` from a clean checkout.
   It builds the release showcase binary once (or takes `--bin`), clones each
   `bench/repos.toml` entry at its exact commit, recovers it through the
   production ingest path and writes `bench-<time>.json`.
2. Run it a second time on the same commit and compare with
   `node scripts/bench-ingest.mjs --compare <first> <second>`. Every count
   (files, nodes, edges, labels, endpoint auth states, receipts and receipt
   omissions) must be identical; the command exits 1 otherwise.
3. Record the Cartograph commit, machine and each repository's wall time, max
   RSS and peak footprint. Compare a change against a run of its base commit;
   report differences in counts as behaviour changes and timing as
   measurements on that machine only.

## MT-SB-01 — Stories render on-brand

1. `cd ui && npm run storybook`.
2. Walk Shell/* and Atlas/* stories.
3. **Pass:** components use the DESIGN.md dark tokens; the four TierBadge
   states are visually distinct (R-INT-2); `Shell/App` stories run their
   interactions without error.

## MT-DF-01 — Dogfood: recover Image Trail end to end

1. `npm run tauri dev`; **Connect** → repo `qwts/image-trail` (or a local
   clone path) and run Preflight → full recovery.
2. Workspace landing: the outcome tally and artifact grid are populated; the
   WebExtension layer row in the ingest summary reports ≥1 manifest.
3. Spec Workbench: `security.md` lists the over-broad optional host grants
   (`http://*/*`, `https://*/*`) with exact scopes; `data_model.md` names the
   IndexedDB stores (history, blobs, bookmarks, …); `gap_register.md` lists
   runtime-computed message identities as explicit Gaps with reasons.
4. Atlas: the Extension node, its contexts (service worker, action), and
   `chan:chrome-message:imageTrail.*` channels are present; a Gap octagon
   opens its Resolution Strategy.
5. Re-ingest the same commit; open **Provenance & Eval** → history.
6. **Pass:** the two ingest rows show identical whole-graph content hashes
   and the determinism footer reads verified (AC-0074, US-0016).

## MT-BB-01 — Large-repo recovery keeps the app interactive

1. `npm run tauri dev`; **Connect** → a local clone of a large real repo
   (thousands of source files — e.g. a production Next.js monorepo).
2. **Preflight**. While it scans, the window stays responsive and the
   status line first reads "Finding files… N so far" while the tree is
   listed, then names the file being read as "Checking file N of M"
   (AC-0197). **Cancel** it, once while it is still finding files and once
   while it is checking them: the scan stops within one directory or file
   and reports
   "Preflight cancelled" with no findings recorded (AC-0198). Run
   **Preflight** again and let it finish.
3. **Run full recovery**. While the Recover stage line is
   visible, immediately: switch surfaces via `⌘1`…`⌘8`, open the command
   palette, and click **Run in background** → the Jobs surface.
4. Throughout the run: the pointer never becomes the macOS beachball, the
   stage label and progress advance, and every surface stays clickable.
5. From Jobs, **Cancel** the run; it stops at the next stage boundary.
6. **Pass:** no "application not responding" episode at any point during a
   multi-minute preflight or recovery (AC-0078, AC-0197, #158, #235).

## MT-HELP-01 — Native Help menu (AC-0090, T-0090)

1. Launch the packaged app (`npm run tauri build` artifact or dev shell).
2. Open the **Help** menu in the native menu bar: it lists **Cartograph
   Help**, **User guide (wiki)**, **Report an issue**, and **About
   Cartograph**.
3. **Cartograph Help** switches the app to the in-app Help view.
4. **User guide (wiki)** and **Report an issue** open in the system
   browser — never inside the webview.
5. **About Cartograph** shows the app name and the version from
   `tauri.conf.json`.

## Planned context-hub acceptance

The following procedures bind T-0106–T-0118 for [SPEC-01](SPEC-01_context-hub.md).
**All H2–H7 acceptance gates remain PLANNED; all procedures below are
UNEXECUTED.** They are future milestone acceptance work, not H1 validation.
Defining a procedure, a passing traceability check, or a partial implementation
does not establish a pass. H1 retains its automated T-0101–T-0105 bindings.

Before running a procedure, record the Cartograph commit/build, platform, target
repository and immutable input identity, configuration, reviewer, and the actual
documented UI/API actions used. Its prerequisites must be implemented and
available; otherwise record **BLOCKED**, with the missing capability and issue.
Do not substitute mock-only results for an end-to-end or real-runtime requirement.
Use disposable local fixtures and synthetic canaries for negative cases. Preserve
redacted transcripts, screenshots, output hashes, and case-level expected/actual
results in the owning milestone task issue; record PASS or FAIL only after all
steps execute. An observed violation is FAIL even if other cases pass.

## MT-H2-01 — Pinned domain recovery and measured coverage (AC-0106, T-0106)

**Gate: H2 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: domain/feature and rule recovery is available; the independent
expected-set review and decision-case matrix in the
[Vendure benchmark](evals/vendure-cart-readiness.md) are complete and frozen.

1. Prepare the benchmark's exact commit and declared default configuration;
   record their identities and the independently reviewed oracle checksum.
   Keep the oracle and corroborating upstream tests outside extraction input.
2. Recover the declared production-source scope twice with identical inputs.
   Open the cart-readiness feature and follow each rule's predicate, configuration
   condition, consequence, and dependency citations to the pinned source.
3. Score all eight expected rules and every frozen decision case using the
   benchmark procedure. Record entry guards and supporting stock rules separately,
   plus extra assertions, duplicates, inferred proposals, missing rules, and gaps.
   Inspect an unsupported dependency path and an unknown runtime input explicitly.
4. Compare rule identities and content hashes between the two runs. Have a
   reviewer explain each rejection and override from the hub and its citations.

**Pass:** all benchmark acceptance gates hold: 5/5 entry guards and 3/3 supporting
rules, zero unsupported confirmed assertions, complete required citations, every
frozen decision case correct, and identical-input determinism. Unsupported paths
and unknown inputs remain visible; inference does not count as confirmed coverage.
**Fail:** any gate misses, including a plausible rule name without its predicate
or evidence. Retain the scored manifest, case decisions, output hashes, coverage
report, and reviewer findings; partial recovery is a measured result, not H2 pass.

## MT-H2-02 — Implementation, intent, inference, and future design (AC-0107, T-0107)

**Gate: H2 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: typed domain views and their provenance display are implemented.
Prepare a pinned fixture with a code guard `quantity > 5`, a design document
stating an intended limit of 10, a cited T3 interpretation, and a proposed future
limit of 20. Include a second feature with code but no supplied design document.

1. Ingest the fixture and open the feature's behavior and design views. Inspect
   each of the four records, its meaning, producing tier/confidence, and citation.
2. Follow the code and document citations. Verify the documented limit is shown
   as written intent despite its conflict with implementation. Review the T3
   interpretation and future design without applying a source-code change.
3. Open the second feature and ask for its design evidence and design gaps.
   Read the corresponding context output as well as the visible explanation.

**Pass:** the four meanings remain distinguishable; neither the document nor an
accepted interpretation proves implementation, and future design remains future.
The second feature reports unknown intent or missing evidence, without asserting
that no design exists. **Fail:** any conflation or tier upgrade. Retain the fixture
identities, four record/citation pairs, and screenshots/context responses for both
the conflicting-intent and missing-document cases.

## MT-H2-03 — Cited local-definition assessment (AC-0171, T-0171)

This source-only analyst procedure can use an independent review agent; record
the reviewer kind and audit scope. It does not execute the human desktop H2
procedures above or establish the complete domain-truth gate.

1. Freeze the [local-definition review plan](evals/vendure-cart-readiness.local-definitions.review-plan.md)
   before output, retaining the existing Vendure oracle, eight-rule denominator,
   41 decision cases, target commit and 17-file input manifest. Verify exact input
   membership and every file hash; keep the oracle, tests and output outside input.
2. At the recorded implementation commit, run the source-only
   `recover_source_rules` example twice into fresh directories. Compare graph,
   bundle, inventory and metadata bytes; record hashes, sizes and revisions in a
   separate local-definitions baseline. Do not install or execute target code.
3. Have an independent reviewer compare the emitted declarations, admitted uses,
   initializer structure and dependencies to original source. The checked-in
   `scripts/audit-local-definitions.cjs` uses the installed UI TypeScript 6.0.3
   parser/binder independently of the producer. Run
   `node scripts/audit-local-definitions.cjs SOURCE_ROOT INPUT_MANIFEST OUTPUT_DIR NEW_AUDIT_JSON`
   with the staged root, frozen manifest and each output directory; retain its
   result and script hash. It never executes target modules or resolves imports.
   Audit the tracking condition and mutated-array negative cases in the frozen
   plan separately. State any syntax or semantics not covered by those checks.
4. Retain separate score JSON and Markdown describing source-evidence gains,
   remaining gaps, unexpected claims and review limits. Preserve historical
   baseline/score files. A changed complete-rule score requires the entire frozen
   oracle's proof; initializer counts alone do not satisfy a rule or decision case.
5. Record captured-parser receipt integrity through its integration regressions
   separately; retrospective source matching by this example is not input-closure
   or production-capture proof. Record the assessment and artifacts on issue #398.

**Pass:** input and oracle remain unchanged, both runs agree, cited gains survive
the stated source audit and no unsupported value/business claim is counted.
**Fail:** changed/contaminated input, unequal output, incorrect source evidence or
inflated rule/case claims. Passing this bounded assessment leaves H2, full #385,
curated context and the market pilot open.

## MT-H3-01 — Shared curation and stale-evidence reconciliation (AC-0108, T-0108)

**Gate: H3 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: durable proposals, shared curated projection, revision-bound source
evidence, and re-ingestion reconciliation are implemented. Prepare two cited T3
proposals against distinct source spans, plus an unaccepted control proposal.

1. Record the recovered graph and proposal identities, producing tiers, and
   evidence basis. Accept the first two proposals through human review.
2. Read the curated UI, context API, and best-effort export at the same revision.
   Match proposal identities and review state across all three. Also read the
   separately addressable recovered view and the verified-only export.
3. Change only the first proposal's cited source in a disposable fixture revision
   and re-ingest it. Inspect both proposals through all three curated surfaces.
   Attempt to reuse the first proposal without reconciling its changed evidence.
4. Perform the documented reconciliation/review flow against the new source and
   read all three surfaces again. Reopen the original revision and review history.

**Pass:** the same accepted projection appears consistently where the export
policy permits it; T3 remains InferredWeak and is excluded from verified-only.
Acceptance never rewrites recovered facts or includes the unaccepted control.
Changed evidence makes the affected proposal visibly stale until reconciled;
unchanged evidence is distinguished, and original provenance/history remains
inspectable. **Fail:** stale content silently remains current, a surface disagrees,
or acceptance upgrades tier. Retain before/after responses, export hashes, review
receipts, source revisions, and the reconciliation outcome.

## MT-H3-02 — Named project isolation across restart (AC-0109, T-0109)

**Gate: H3 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: persistent named contexts, domain membership, review history, and
task references are implemented. Prepare projects A and B with the same relative
path and symbol names but different rule values and immutable input identities.

1. Create named contexts A and B. In each, ingest its fixture, associate a domain,
   make a distinct review decision, and retain a task reference. Record all IDs.
2. Switch A → B → A. Inspect domain evidence, source revision, decisions, and task
   references in each; follow the links to their owning records and sources.
3. Quit fully and relaunch. Repeat the inspection, then re-ingest a changed
   revision of A and switch to B again.
4. Through the documented context interface, request A's scoped record/task while
   selecting B. Verify it is rejected or explicitly identified as outside B,
   rather than rebound to B's same-named source.

**Pass:** names and records survive restart, every link retains its actual project
and revision, and A's update/reviews do not alter B. **Fail:** missing history,
silent project substitution, or cross-project evidence/task leakage. Retain the
project/revision/record matrix and responses before and after restart and update.

## MT-H4-01 — Shared durable investigation coordinator (AC-0110, T-0110)

**Gate: H4 PLANNED; MCP entry requires H5 support. Procedure: UNEXECUTED.**

Prerequisites: app and MCP investigation entry points and the shared durable
coordinator are implemented. Configure a named/versioned specialist, permitted
feature scope, tier ceiling, and finite time/token/tool budgets. Declare the
task-start latency bound before the run.

1. Start one permitted investigation in the app and a second through MCP against
   the same project/revision. Record request-to-task-ID latency for both.
2. Retrieve each task from both interfaces. Compare its durable ID, project,
   revision, scope, named agent/version, tier ceiling, input references, budgets,
   origin, and parent identity (absent for these root tasks).
3. Let the tasks complete; read their cited findings and task status through both
   interfaces. Restart the app and retrieve both tasks again by their original IDs.

**Pass:** both ingress paths use the same durable task records and enforce the
declared limits; IDs arrive within the declared bound without waiting for model
completion, and metadata/results remain consistent after restart. Distinct starts
need not share one ID. **Fail:** transport-local task copies disagree, required
metadata is missing, or task identity/results are lost. Retain the request/status
transcripts, durations, task records, and restart evidence.

## MT-H4-05 — Separately gated V7 completion control (AC-0267, T-0267)

**Issue #652. Procedure: UNEXECUTED. H4/H5 remain PLANNED.**

The frozen [revision 2 protocol](acceptance/v7-completion-control-v2.md) has SHA-256
`6f584efbfa7f91a002eea4735294096a87979e4e4ffff618d92d00f09ad911e7`.
Its historical draft header is preserved byte-for-byte. This procedure implements
that protocol; neither the document nor a successful scripted test authorizes a
provider invocation. Integrate and review the prerequisite stack before execution.
Retain all earlier V2–V6 outcomes.

1. Obtain the owner's explicit run authorization and verify the already installed
   Ollama model/runtime read-only. Freeze a clean implementation commit/tree,
   runtime/model identity, sampling settings (including explicit unknown/unset),
   and an independent semantic rubric outside the run and capture directories.
   Preparation creates a new exclusive run directory, a queued analyst and both
   independently resolved mixed request envelopes. It makes no provider call.
   Publish the frozen definition's SHA-256 before any execution.
2. Select only the analyst stage with that externally recorded definition digest.
   The stage acquires an exclusive attempt marker before invoking the ordinary
   worker. Preserve the marker and all evidence even if execution fails. Never
   delete a marker, edit a frozen definition, or reuse the run directory to retry.
   The worker's first-refusal continuation uses its ordinary shared budget; it
   does not authorize a harness retry. The analyst stage seals its receipt and
   exits without creating an auditor. A post-dispatch recording failure preserves
   captured observation hashes in a separate invalid-run artifact and produces no
   eligible receipt. If storage itself cannot accept that artifact, retain the
   attempt marker and coordinator database and report the write error; never
   treat missing output as permission to repeat the invocation.
3. Give Claude the frozen rubric and admissible evidence, without Codex's semantic
   interpretation. Claude creates a new comment on issue #652 containing exactly
   one JSON block with schema `cartograph-v7-semantic-review@1`, `protocol_sha256`,
   `run_id`, `role`, `receipt_sha256`, `rubric_sha256`, `verdict` and `reasons`.
   Bind `receipt_sha256` to the outer sealed receipt, which also binds the runtime
   receipt and evidence artifact. A local JSON file cannot prove reviewer identity.
4. Immediately before an authorized auditor invocation, authenticate and freshly
   fetch that comment through `scripts/verify-v7-semantic-review.mjs` with
   `--binding <binding.json> --comment-id <id> --output <new-attestation.json>`.
   It verifies the Codex bot viewer and Claude App/comment author through REST and
   GraphQL, exact issue and hash bindings, and that the comment was never edited.
   The selected verdict must be the latest matching Claude App review for the same
   run, role and protocol/receipt/rubric hashes, found by bounded paginated listing.
   Deleted or edited comments cannot gate execution; corrections only append a new
   comment. Reviewers must never delete verdicts: deletion history is unavailable.
   Metadata changes that alter the comment timestamp also fail closed.
   Preserve the immutable attestation separately from the analyst receipt.
5. Select the auditor stage only after the analyst's mechanical checks and Claude's
   independent semantic review both pass, with the same frozen definition digest.
   The harness verifies local content bindings; it does not authenticate arbitrary
   local attestations or grant the owner's authorization. Revalidate the auditor's
   independent obligations, execute once, seal its evidence, and obtain a separate
   authenticated Claude semantic review using the same procedure.
6. Report first-shot supported and supported after one refusal separately.
   Insufficient evidence, unmet obligations, failed, cancelled, exhausted, unknown,
   and unreviewable outcomes cannot pass this control. Record skipped stages.
   A mechanical pass alone is not a semantic pass; delivery remains false.

The ignored entrypoint is `investigation_v7_completion_control_explicit_stage`.
Invoke each stage through `cargo test -p app
investigation_v7_completion_control_explicit_stage -- --ignored --nocapture`,
never by executing a previously compiled test binary directly. This ensures Cargo
rebuilds the reviewed source before the checkout identity check. Normal tests
compile it without invoking Ollama. Select one stage using
`CARTOGRAPH_V7_STAGE` (`prepare`, `analyst`, or `auditor`) and an absolute
`CARTOGRAPH_V7_RUN_DIR`. Preparation additionally requires `CARTOGRAPH_V7_RUN_ID`,
`CARTOGRAPH_V7_RUNTIME_RECEIPT`, and `CARTOGRAPH_V7_RUBRIC`. Each execution stage
requires `CARTOGRAPH_V7_DEFINITION_SHA256` from the published preflight and
`CARTOGRAPH_V7_OPERATOR_OPT_IN` equal to that single role. Auditor execution also
requires `CARTOGRAPH_V7_SEMANTIC_ATTESTATION`. These environment variables select
behavior; they do not authenticate authorization.

The external runtime receipt is a closed JSON object with `schema_version: 1`,
`endpoint`, `model`, `model_digest` (64 lowercase hex characters), `runtime_version`,
`temperature`, `seed`, `context_length`, and `verified_read_only: true`. Record
sampling fields as explicit null when unset or unverified. The harness preserves
this external observation without overriding provider sampling or inventing
runtime defaults. Verify it read-only immediately before the authorized run.

This n=1 synthetic control does not establish a success rate, Vendure acceptance,
native restart, or disposable-capture forgetting. Coordinator reopen checks do
not establish those distinct gates. A proved harness defect requires preserved
evidence and a separately reviewed, authorized replacement definition; a failed
model result is not a reason to replay.

## MT-H4-02 — Controlled local specialist investigation (AC-0191, T-0191)

**Increment #404. Procedure: UNEXECUTED. H4/H5 remain PLANNED.**

Prerequisites: a reachable loopback Ollama runtime and pinned model; production
coordinator/source-reader/bounded-provider integration; a pinned captured
TypeScript fixture with an independently reviewed expected answer kept outside
all model inputs. Record runtime/model identity and fixture/capture hashes before
starting. Do not provision cloud access as a substitute for this local procedure.

1. Start a Domain analyst investigation over that fixture with a scoped question.
   Record its durable task ID, request-to-ID latency, immutable specialist/prompt,
   provider/model, graph/scope snapshot and declared limits. Retrieve ordered
   events while the actual provider chooses query, evidence-read and finish steps.
2. Independently inspect the supplied input ledger and saved citations against the
   retained exact source occurrences. Compare each finding to the withheld
   expected answer; record unsupported claims, omissions and coverage limitations.
   Check raw source replay is absent and all generated findings remain T3/weak.
3. Run an Evidence auditor follow-up against the selected parent's saved result.
   Verify its history retains original scope, status, revision and uncertainty;
   no current-source lookup silently replaces the parent's historical basis.
4. Restart the app and reopen both task IDs and their historical citations. Change
   or remove the checkout, and verify retained source is still the cited source.
   Use a disposable capture to verify forgetting reports unavailability without
   changing the saved finding or opening current source.
5. Record actual calls, tool actions, read attempts, validation bytes, generated
   token reservations, provider-reported usage (or unknown), elapsed time and any
   limits/failures. Retain the journal and independent citation review on #404.

**Pass:** the actual local model drives the production query/read/finish loop,
results are evidence-supported within explicit coverage, and durable identity and
citations survive restart and source changes. **Fail:** scripted/fabricated output
substitutes for a model run, an oracle enters model input, source or authority is
silently rebound, limits are bypassed, or unknown outcomes are replayed.

A production-module harness run may establish coordinator/local-provider evidence
with separately CI-tested UI behavior; it is not native-app or external-MCP
end-to-end evidence. MT-H4-01 and all H5 cross-ingress procedures remain unexecuted
until their actual prerequisites and steps are run.

The ignored native test `investigation_local_provider_acceptance` retains the
legacy @2 analyst/auditor control. The separate
`investigation_compact_local_provider_acceptance` selects the compact @3 pair;
`investigation_turn_progress_local_provider_acceptance` selects explicit @4 turn
progress. Each runs the actual Ollama provider through the production worker and captured-source reader. It uses
a disposable fixture and private coordinator, never the developer's app data.
Run it from an owner-operated terminal with an already installed local model;
agents follow the governed remote-heavy-suite policy. No model download, cloud
fallback, hidden repair or replay is performed. Normal CI compiles this test and
leaves it ignored.

```sh
CARTOGRAPH_ACCEPTANCE_URL=http://127.0.0.1:11434/ \
CARTOGRAPH_ACCEPTANCE_MODEL='<installed model name>' \
CARTOGRAPH_ACCEPTANCE_MODEL_DIGEST='<digest from the local model inventory>' \
CARTOGRAPH_ACCEPTANCE_RUNTIME='<runtime version and hardware>' \
CARTOGRAPH_ACCEPTANCE_OUTPUT='/absolute/path/to/a/new-run-directory' \
cargo test -p app investigation_local_provider_acceptance -- --ignored --nocapture
```

For compact @3 or explicit @4 turn progress, use the same environment with the
corresponding exact test name above and a different new output directory. Choose the specialist version before starting; a new version is a
separate observation, never a replay or replacement for a failed control. All
three entrypoints use the same synthetic fixture, limits and acceptance checks;
none is the Vendure primary-input procedure.

The separately selected
`investigation_source_inspection_local_provider_acceptance` exercises @5 with
explicit captured-source requirements. Use the same environment and its exact
test name with a fresh output directory. The analyst and auditor each receive an
independently host-resolved `definition_initializer` target from the existing
synthetic source. Both intended targets and the source content hash are recorded before the first
model call. The auditor target is revalidated before its follow-up starts, and
the report records that outcome even when it succeeds. This does not
inherit requirements from parent findings or force a source-reading tool call.

The @5 report additionally checks the exact saved requirements and their finishing
assessment: each target must have been acquired and supplied unchanged, with the
assessment bound to the finishing invocation and input ledger. Citation support
remains a separate check. A valid partial result with unmet requirements fails
mechanical acceptance and is retained; the auditor is skipped after a failed
analyst control. Existing @2–@4 controls and their receipts are unchanged. This
@5 control is a production-module observation, not a native-app run, a Vendure
acceptance result, or proof that model conclusions follow from the source.

The @5 entrypoint can also run an owner-hosted OpenAI-compatible server (vLLM,
LiteLLM, llama.cpp) by adding `CARTOGRAPH_ACCEPTANCE_PROTOCOL=openai-compatible`;
the default is `ollama`. The URL is the server root (the provider appends
`v1/chat/completions`) and must be loopback, so reach another machine through an
explicit tunnel such as `ssh -fN -L 18000:127.0.0.1:8000 <host>`, then use
`http://127.0.0.1:18000/`. Such a server reports no weight digest: record the
served model root or another operator-checked identity as the digest, which the
report keeps as operator-supplied and unverified.

The ignored `investigation_qualified_local_provider_acceptance` entrypoint selects
@6. In addition to the common environment above, set
`CARTOGRAPH_ACCEPTANCE_RUBRIC` to the absolute evaluator-only rubric path and
`CARTOGRAPH_ACCEPTANCE_RUNTIME_EVIDENCE` to the absolute read-only runtime identity
receipt. Set `CARTOGRAPH_ACCEPTANCE_MAILBOX` to the existing absolute mailbox
directory where the frozen manifest will be published. The acknowledgement must
resolve inside that directory; an external copy elsewhere is insufficient.
Set `CARTOGRAPH_ACCEPTANCE_GATE` to the new output directory's
`publication.json`; do not create that file until the frozen manifest has been
published and its exact bytes verified. The bounded publication wait fails closed
without a provider call if the acknowledgement is absent or invalid. The
acknowledgement is a strict JSON object with `schema_version: 1`,
`manifest_hash` equal to the frozen manifest's BLAKE3-256 hash, and
`mailbox_manifest` naming an absolute external copy of its exact bytes. The
harness rechecks every frozen artifact after acknowledgement; publishing a changed
manifest or modifying an input does not authorize dispatch. SHA-256 hashes used
in review messages are separate from these BLAKE3 artifact identities.

The separately versioned @6 synthetic coverage control additionally requires an
actual inventory read and the same frozen coverage identity on every invocation.
Its inventory is explicitly harness-generated and published through the production
graph-and-coverage transaction using the real active-plugin configuration hash.
Existing producer fields carry that marker; inventory metadata cannot become a
source citation or satisfy the independent source-inspection obligation.

Before dispatch, freeze the questions, source and inventory bytes, specialist
identities, context identity, and evaluator-only rubric in a new artifact directory.
Publish their hashes before any provider request. Keep the rubric outside captures,
model inputs, tool responses and parent history. Preparation may preview the
production context; the persisted request must bind its expected identity, and
actual worker preparation must reject a changed identity. Publication waiting occurs
before the worker starts, without pausing or extending its work limits.

Record mechanical and semantic outcomes separately. Check each actual sealed
invocation against the frozen qualification, and require the auditor's own source
and inventory observations. Bind the auditor's actual supplied history to the saved
parent result ID and body hash; nonempty history alone is insufficient. Compare
source-span hashes with disclosed text hashes
and inventory-body hashes with disclosed body hashes; full-file and envelope hashes
are different domains. If redaction changes a supplied representation, review that
actual representation or mark its claims unreviewable. An initializer-only source
span cannot establish the containing function's full behavior. Preserve failures,
skip the auditor after a failed analyst control, and perform no hidden repair or
retry. This control does not replace prior @5 observations, the Vendure primary
procedure, native restart, destructive forgetting or independent citation review.

Use a clean checkout and an output directory outside it that does not exist. The
manifest records the exact source commit and intended analyst/auditor definitions
before the first provider call. It records runtime/model identity
as operator-supplied evidence, not a weight-content attestation; independently
check it against the runtime inventory. The harness retains the fixture, private
state/capture directory and `run.json` even when model acceptance fails. It checks
real query/read/finish activity, captured citation readability with the checkout
removed, unchanged graph authority, a separately requested auditor follow-up,
and coordinator reopen equality. Read the journal and observed model identity;
unknown usage remains unknown. Connection reopen is not a native app restart.

Review the results against [the withheld fixture oracle](acceptance/specialist-guard-oracle.md).
That file is never supplied to either model. The report always leaves independent
citation review, native restart and destructive forgetting checks pending. A
passing harness assertion alone does not execute all of MT-H4-02 or pass AC-0191.

**Vendure capture preparation only (#598).** The ignored
`vendure_capture_preparation` entrypoint registers the frozen 17-file production
source closure at Vendure `2cc309deb5394af176314552ac7320a7b26a0104` and runs the
production capture/ingest pipeline. It verifies the exact
`docs/evals/vendure-cart-readiness.input.json` manifest and separately retained
Git-blob input proof, then records actual capture hashes, receipt/range candidates
and producer coverage. Keep the proof, oracle, review material and reports outside
the registered source root. Do not point it at the Git object cache or include
adjacent evaluator files in the source tree.

Use a clean checkout. All paths below must be absolute; use canonical paths
without symlink aliases. The output directory must not exist, must have an
existing canonical parent, and must be outside the source root and code checkout.
The source, disposable profile, output and code-checkout roots must be mutually
disjoint canonical paths. The manifest, Git-blob proof and native-config JSON
must be canonical external files outside source, profile and output roots; these
boundaries are rechecked around ingestion and report collection.
The native-config JSON must contain
`productName`, an `identifier` beginning `com.qwtm.cartograph.acceptance598.`, and
this explicit directory map:

```json
{"productName":"Cartograph Acceptance 598 Isolated","identifier":"com.qwtm.cartograph.acceptance598.isolated","app":{"appDirectoriesOverride":{"data":"/absolute/new-profile/data","config":"/absolute/new-profile/config","localData":"/absolute/new-profile/local-data","cache":"/absolute/new-profile/cache","log":"/absolute/new-profile/logs"}}}
```

The canonical disposable profile root must be separate from the source and code
checkout, with an existing parent; it may be absent or an empty directory. Its five
mapped subdirectories must not exist. Never reuse normal app data or another run's
profile. No provider environment variables or installed model are needed.

```sh
CARTOGRAPH_VENDURE_SOURCE='/absolute/staged/source' \
CARTOGRAPH_VENDURE_MANIFEST='/absolute/evaluator/vendure-cart-readiness.input.json' \
CARTOGRAPH_VENDURE_PROOF='/absolute/evaluator/input-proof.json' \
CARTOGRAPH_VENDURE_NATIVE_CONFIG='/absolute/evaluator/native-config.json' \
CARTOGRAPH_VENDURE_PREPARATION_OUTPUT='/absolute/new-preparation-directory' \
cargo test -p app vendure_capture_preparation -- --ignored --nocapture
```

Retain the exclusive `preparation.json` or `preparation-failed.json` and the
disposable profile in place. No provider is constructed or called; no native app
is launched. Candidate metadata requires later caller selection and establishes
no input closure. This preparation does not pass AC-0191. Discovery/frozen-dispatch
review, actual analyst and independent auditor runs, withheld-oracle semantic and
negative-case review, native restart/source-removal checks and disposable capture
forgetting remain pending. A later native handoff must use the same recorded
directory map and databases after harness handles close, without copying or
rebasing databases.

**Vendure selection measurement (#686).** Keep the preparation receipt and the
same disposable profile in place. Measurement reopens those production stores;
it must not recreate the profile, register another source, copy databases or
substitute synthetic coverage. Select exact fact/receipt/range ordinals from the
preparation catalogue for the analyst and auditor independently. Questions and
selection metadata must remain separate from the withheld oracle.

The measurement receipt distinguishes a proposed request from an executable,
admitted request. It records each role's production-resolved target identities,
request serialization size, source-range and full-file validation costs,
inventory response size, and minimum source/inventory actions plus a separate
finish invocation. It includes the measurement and preparation code revisions, their equality
status, the compared profile ID and exact limits, and reports exceeded limits
explicitly. An over-budget proposal is never staged as a
task. These counts are lower bounds: discovery, unsuccessful reads and additional
model turns require headroom. They do not establish semantic sufficiency.

Use the ignored `vendure_measure_prepared_selections` entrypoint only after
operator authorization. Its selection JSON is an array with exactly two entries,
`domain-analyst@7` followed by `evidence-auditor@7`. Each entry contains
`specialist_id`, `question`, `request_nonce`, `scope`, and a nonempty `sources`
array. Each source contains `fact`, `graph_snapshot_id`, `receipt_id`, and the
zero-based receipt inventory `ordinal`, copied from the actual preparation
catalogue. Use the production scope/fact JSON forms. Optional `parent_id` and
`conversation_id` must describe existing intended bindings; do not fabricate
future analyst IDs. Missing future auditor bindings mean request byte counts
measure the proposal only, not the eventual executable auditor request.

Keep the configuration, preparation receipt and selection file canonical and
external to the source, profile and new measurement-output roots. The profile
must still contain its original production stores. The output directory must
not exist and must have an existing canonical parent.

```sh
CARTOGRAPH_VENDURE_NATIVE_CONFIG='/absolute/evaluator/native-config.json' \
CARTOGRAPH_VENDURE_PREPARATION_RECEIPT='/absolute/preparation/preparation.json' \
CARTOGRAPH_VENDURE_SELECTIONS='/absolute/evaluator/selected-targets.json' \
CARTOGRAPH_VENDURE_MEASUREMENT_OUTPUT='/absolute/new-measurement-directory' \
cargo test -p app vendure_measure_prepared_selections -- --ignored --nocapture
```

Retain `measurement.json` or `measurement-failed.json` with the selected-target
input and original preparation receipt. The output directory is reserved and
pinned before stores are opened. A boundary or binding change can prevent even
a failure receipt from being written safely. The manual entrypoint rechecks the
frozen Vendure repository, commit and input-proof identity before measuring;
offline fixture receipts cannot substitute for a Vendure observation. Both roles must be measured independently, even when they select the
same source ranges. The receipt's comparisons concern the recorded profile and
limits, not a proposed expanded profile.

Measurement does not change limits, call a provider or authorize an analyst or
auditor run. Retain its immutable metadata-only output outside source/profile
roots. A real preparation and measurement invocation requires its own operator
authorization; offline fixture tests are separate evidence. Review the observed
numbers before proposing a product-level budget profile. T-0191 remains
UNEXECUTED until the complete controlled workflow has actual evidence.

For a governed remote run, dispatch the existing CI workflow on the exact branch
with purpose `exact-sha-preflight` and `local_provider_acceptance=true`. This
explicit opt-in still runs the legacy @2 control after the full Rust suite, on the same
GitHub-hosted Linux runner. PR, queue, main-push and ordinary manual CI events do
not install or invoke a model. The opt-in test is part of the Rust job, so a failed
model run fails that CI run and still uploads its diagnostic evidence.

The runner script pins [Ollama v0.34.0](https://github.com/ollama/ollama/releases/tag/v0.34.0)
by release-archive SHA-256 and [Qwen3 8B](https://ollama.com/library/qwen3:8b) by the
full model-manifest digest. It verifies the installed runtime version and model
inventory before invoking the production provider. Inference is loopback-only;
Ollama cloud features are disabled. Downloading the runtime/model is runner setup,
not application behavior or a download performed by Cartograph. The context length,
CPU/memory, pins and observed identities are recorded. Limits and model actions
are unchanged; slow or invalid model output is a failed observation, not a reason
to synthesize a response or replay an unknown task. The artifact contains the
synthetic fixture and coordinator evidence, not model weights or raw responses.

## MT-H5-01 — ACP capabilities, environment, and egress (AC-0111, T-0111)

**Gates: H4–H5 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: ACP negotiation, coordinator policy, enforced runtime filesystem
boundary, and per-tier consent are implemented. Use a compatible local runtime,
a disposable read-only target, a permitted output directory, and synthetic
canaries outside both. Enable tool/runtime/network audit capture.

1. Record negotiated protocol/runtime capabilities and configured tool, path,
   tier, and egress permissions. Run a permitted context read and output write.
2. Request an unnegotiated tool, a target-code write, an out-of-scope canary read,
   and a confirmed-fact write. Repeat the requests when instructions embedded in
   repository text claim to grant those permissions. Compare target hashes and
   confirmed graph contents before and after.
3. Under local-only policy, request cloud execution and verify no cloud runtime
   launch or outbound payload occurs. Exercise the opted-in test provider with
   denied consent, then with consent for one exact redacted payload and tier;
   change that payload or tier and retry without new consent.
4. Complete an allowed investigation and inspect its returned proposal citations
   and confidence ceiling. Check the audit for tools/paths used during execution.

**Pass:** only negotiated and authorized operations execute; denied reads do not
return canaries, source/confirmed facts remain unchanged, and egress requires
matching per-tier consent. Allowed results are cited proposals. **Fail:** any
unauthorized execution, disclosure, egress, or fact upgrade, even if the runtime
reported a permission dialog. Retain redacted negotiation/denial/network records,
hash comparisons, consent receipts, and the successful proposal.

## MT-H5-02 — Cancellation, disconnect, and uncertain runtime outcomes (AC-0112, T-0112)

**Gates: H4–H5 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: durable task lifecycle and runtime reconciliation are implemented.
Use a controllable runtime that exposes invocation IDs and can pause before work,
during work, and after producing a result but before host acknowledgement.

1. Start a task, wait for progress, and cancel it. Observe the runtime cancellation
   receipt and terminal state; query that task after reconnecting and restarting.
2. Start a second task, disconnect only the caller during work, and reconnect.
   Retrieve its progress and eventual result by the original ID; confirm the
   reconnect did not create another runtime invocation.
3. Start a third task and terminate the app while the runtime is paused before
   acknowledging its outcome. Relaunch and inspect the persisted task. Request a
   retry before reconciling the runtime invocation, then reconcile it using the
   documented recovery flow and retrieve the resulting terminal record.
4. Repeat the crash case with a runtime that cannot establish whether it finished.
   Inspect the unresolved state and attempted retry behavior.

**Pass:** progress/terminal records remain queryable, cancellation reaches the
runtime, and disconnect/restart do not silently duplicate work. An uncertain
outcome stays explicit and prevents automatic retry until reconciled; an
unreconcilable outcome is not invented as success. **Fail:** lost outcomes,
unpropagated cancellation, or duplicate invocation without reconciliation. Retain
task and invocation IDs, progress/cancel transcripts, restart records, and the
recovery decision for each case.

## MT-H5-03 — Delegation ancestry, deduplication, and shared limits (AC-0113, T-0113)

**Gate: H5 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: nested MCP/ACP delegation and coordinator accounting are implemented.
Configure a depth limit of two descendant levels, finite shared tool/token/time
budgets, and a reproducible worker that requests specified child investigations.

1. Start a root MCP investigation. Have its ACP worker create a permitted child;
   read both records and verify project/revision, parent ancestry, and budget
   accounting remain connected to the root.
2. Submit the same child request concurrently and again after reconnect, using
   the documented deduplication identity. Count actual runtime invocations.
3. Attempt direct self-delegation, A → B → A ancestry recursion, and a chain
   extending beyond the configured depth. Inspect returned errors and task records.
4. Run two valid children whose combined requests exceed each shared budget in
   turn. Inspect the root/child usage ledger and any attempted later delegation.

**Pass:** duplicate requests do not duplicate execution, cycles and excess depth
are rejected explicitly, and children cannot reset or multiply their shared
allowance. Exhaustion stops further work with the responsible bound visible.
**Fail:** an unbounded loop, missing ancestry, or execution beyond the configured
enforced limits. Retain the task tree, deduplication keys, invocation counts,
configured limits, usage ledger, and denial records.

## MT-H5-04 — Real external MCP caller and ACP runtime (AC-0114, T-0114)

**Gate: H5 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: a released external development application's MCP caller, a real
compatible ACP runtime, and Cartograph's documented connection instructions.
Record all product/runtime versions, negotiated capabilities, and finite budgets.
Mock protocol fixtures alone do not satisfy this procedure.

1. Connect the external caller and read a scoped Cartograph context page. Follow
   a citation and compare its project/revision with the app's corresponding view.
2. Start an ACP investigation through MCP. Record its durable task ID and observe
   real progress while it runs; disconnect/reconnect the caller and retrieve the
   completed cited result. Inspect the same task in Cartograph.
3. Start a separate investigation, observe progress, cancel from the caller, and
   retrieve its terminal outcome after reconnecting. Verify runtime cancellation.
4. Record whether MCP Tasks was negotiated. Execute these lifecycle steps through
   negotiated Tasks support when present; also exercise the explicit
   start/status/result/cancel compatibility path with Tasks disabled or a second
   real caller that does not negotiate it. Record the actual tool names used.

**Pass:** the real caller completes context read, investigation, progress,
reconnect, cancellation, and cited-result retrieval with the same durable records
as the app; the explicit lifecycle baseline also works without Tasks support.
**Fail:** any required step needs a mock or loses identity/provenance. Retain
redacted connection instructions, negotiation and lifecycle transcripts, runtime
invocation IDs, and the cited result; name any incompatible capabilities.

## MT-H6-01 — Cited structural measurements and missing coverage (AC-0115, T-0115)

**Gate: H6 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: feature-scoped structural evaluation is implemented. Prepare a
small pinned multi-file feature with a known dependency cycle and independently
counted size, fan-in/out, and cohesion-proxy expectations under the documented
metric definitions; include available Git history and an external dependency.

1. Evaluate the declared feature twice at the same revision and budget. Compare
   every reported measurement with the independently calculated values and follow
   its source/revision citation. Record the selected and excluded scope.
2. Repeat with dependency evidence unavailable, and separately with history
   unavailable. Preserve the same source content and record changed input coverage.
3. Inspect measurement, coverage, and confidence fields independently. Attempt to
   follow a prior-revision citation after re-ingesting a changed fixture revision.

**Pass:** measurements match their documented definitions and pinned evidence;
identical inputs agree. Missing dependency/history inputs are explicitly reported
as incomplete coverage, not zero dependencies/changes or a confidence substitute.
Old evidence remains tied to its revision. **Fail:** incorrect counts, uncited or
misbound measurements, or hidden coverage loss. Retain the independent worksheet,
input/output identities, and full versus incomplete-coverage reports.

## MT-H6-02 — Contextual responsibility hotspots (AC-0116, T-0116)

**Gate: H6 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: structural evaluation and the architecture specialist are
implemented. Before viewing its output, a reviewer selects two comparably large
files: one cohesive unit with a documented reason to remain together, and one
combining distinct responsibilities with documented coupling concerns. Freeze
their revision, responsibilities, constraints, and expected supporting evidence.

1. Evaluate each file in its feature/dependency scope with the same declared
   budgets. Inspect the measurements separately from the specialist's judgment.
2. For every hotspot assertion or exception, follow citations for responsibility,
   cohesion, coupling, and design constraints; compare them with the frozen review.
3. Remove the design document from a disposable input variant and repeat. Inspect
   whether the specialist acknowledges unknown constraints and revises uncertainty.

**Pass:** size alone yields no confirmed god-file claim. The justified large file
is not condemned solely for its size, and the responsibility hotspot is assessed
using cited context with scope, uncertainty, impact, and remedy. Interpretations
retain their inferred tier; missing constraints remain unknown. **Fail:** a
size-threshold-only verdict, invented rationale, or confirmed subjective judgment.
Retain both reports, the independent comparison, and the missing-document result.

## MT-H6-03 — Approved design, separate implementation, and re-ingestion (AC-0117, T-0117)

**Gates: H6–H7 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: future-design review and before/after comparison are implemented.
Freeze a baseline repository revision, approved future design, and executable
acceptance cases. Obtain separate human authorization naming the external coding
agent, disposable implementation checkout, allowed change, and permitted commands.

1. Save the original recovered evidence and approved design/review IDs. Have the
   separately authorized external agent implement the change in its checkout.
   Confirm Cartograph's analysis itself has not written the ingested target.
2. Execute the approved acceptance cases against the changed checkout and retain
   observed results, command/runtime identities, and the tested source revision.
   Re-ingest that exact revision and compare it with the baseline and design.
3. Repeat the comparison on a controlled failing variant that violates one
   acceptance case. Also inspect a variant with no observed test result.
4. Reopen the original evidence and review history after both comparisons.

**Pass:** before/after evidence remains distinct and tied to actual revisions;
the report maps the change to approved design and observed tests, identifies the
failing case, and keeps unobserved behavior unknown. It never silently asserts
complete behavioral equivalence or rewrites original evidence. **Fail:** missing
authorization boundary, target writes from analysis, lost baseline, or a success
claim contradicted by a failing/missing test. Retain authorization, design/AC
references, diffs, test records, and both comparison reports.

## MT-H7-01 — Independent market pilot with recorded outcomes (AC-0118, T-0118)

**Gate: H7 PLANNED. Procedure: UNEXECUTED.**

Prerequisites: the preceding capabilities needed by the pilot are implemented;
an independently recruited new user has an authorized repository and the normal
distribution/onboarding instructions. Before observing results, freeze the pilot
scope, independent answer rubric, setup and investigation time/cost thresholds,
acceptable coverage, citation/accuracy targets, and improvement acceptance cases.

1. Observe the user install/configure Cartograph and ingest the chosen revision
   without operator intervention. Time setup and record every failure, recovery,
   credential prompt, coverage explanation, and instance of assistance separately.
2. Have the user find the feature breakdown and business rules for one domain,
   distinguish implemented behavior from documented/future design, identify design
   gaps, and investigate one architecture concern. Record elapsed time, runtime
   and token use/cost, the exact answers, citations, and disclosed uncovered scope.
3. Have an independent reviewer score those answers against the frozen rubric,
   following citations to the actual source. Record correct/total answers,
   supported/total assertions, uncovered cases, and disagreements.
4. Have the user select one improvement, review its design/acceptance criteria,
   and complete the separately authorized implementation and re-ingestion flow
   in MT-H6-03. Record test results, evidence comparison, and the user's review.
5. Compare observed setup, answer quality, coverage, time/cost, and improvement
   results with the preregistered thresholds. Record the user's unresolved friction
   and any follow-up issues before deciding this pilot's result.

**Pass:** the new user completes the workflow independently, meets the frozen
thresholds, and reviews one improvement supported by observed acceptance evidence.
**Fail:** assistance was required for completion, a threshold misses, evidence is
unsupported, or the improvement is unreviewed. Retain the anonymized session
record, scored rubric, measurements, and improvement artifacts. This procedure
records one pilot; broader market readiness still requires the several external
users and launch requirements named in SPEC-01, rather than extrapolating from
one successful participant.


## MT-H4-03 — Explicit captured-source inspection requirements

**Status: PLANNED / UNEXECUTED.** This procedure covers AC-0261; it does not
replace real-provider acceptance or certify the truth of generated prose.

1. In a disposable captured-source project, select a known Atlas fact and range.
   Add that range as a required inspection; verify its human-readable path/range,
   remove and re-add it, and explicitly choose a V5 specialist. Merely viewing
   source must not discharge the requirement. Confirm the 8,192-byte total request
   limit can prevent adding/submitting fewer than the maximum 12 targets; sources
   must not be silently dropped to fit.
2. Submit the request, retain its immutable target and task identities, and inspect
   acquired/supplied/cited assessment independently of execution status. A model
   that finishes without source inspection must retain its partial result with an
   unmet requirement. Do not retry or force its next action.
3. With scripted boundary tests separately, verify wrong-range and withheld
   evidence remain unmet, supplied-but-uncited is distinguished, and repeated reads
   do not inflate unique coverage. Scripted checks are not real-model acceptance.
4. Reopen saved detail and follow-up history. Confirm stable original assessment,
   legacy not_assessed, terminal V5 not_assessed when no result was produced, and
   no silent inheritance of requirements. Begin a V5
   follow-up, enter a question, navigate to Atlas to select a new required range,
   and return: parent, conversation, question and explicit specialist selection
   must survive, while old requirements are not inherited. Change a source
   binding between selection and submission: admission must require refreshing,
   never retarget the requirement silently.

**Pass:** all observed identity and assessment boundaries hold, with evidence
recorded for the exact source revision. **Fail:** metadata, user viewing, another
range or parent history is presented as a current source inspection.

## MT-H4-04 — Exact captured-source structured locations

**Status: PLANNED / UNEXECUTED.** This procedure covers AC-0262.

1. Select retained source with known byte offsets and multiple lines, including a
   UTF-8 multibyte character. Compare structured byte/line/column locations with
   that exact captured buffer and document the declared indexing convention.
2. Alter or remove the working-tree file and reopen the captured source. Locations
   must stay tied to the original retained source and receipt identity.
3. Exercise a receipt whose inventory ordinal differs from its original role
   index. The selected target must resolve the actual role/index; the ordinal
   must not be sent as a tool index.
4. Verify malformed/out-of-range references fail closed. An opaque identifier
   suffix such as @132 cannot become line132. A model's arbitrary prose location
   claim still needs independent quality review.

**Pass:** structured locations and selected target identities match the exact
captured source. **Fail:** live file text or opaque IDs determine the locations.

## MT-H4-06 — Observe installed Studio prerequisites, optionally prepare V7 (AC-0279, T-0279)

**Procedure: UNEXECUTED.** This operator-only helper does not authorize a provider
run. Read-only observation uses an already established credential-free loopback
forward or execution on the explicitly asserted Studio
`mac14x13-m2max-32g-60f1` (optional `.local` or `.lan`). It creates no tunnel and cannot prove
which host serves loopback. Verify that path independently. Do not provision or
substitute a model. Observation is not a later dispatch identity guarantee.

1. Select the reviewed checkout, expected installed runtime version/model/digest,
   future absolute V7 run directory, and a new absolute observation directory
   outside that run and all declared capture/model-visible roots. Supply those
   roots explicitly. Keep the independent rubric outside all those roots and
   model inputs. The collector admits only HTTP loopback origins without URL
   credentials, query or fragment and issues GET `/api/version` and `/api/tags`.
2. Use `node scripts/collect-v7-runtime-prerequisites.mjs` with `--checkout`,
   `--endpoint`, `--model`, `--expected-runtime-version`, `--expected-model-digest`,
   `--target-host`, `--observation-path` (`on-studio` or `operator-established-forward`),
   `--confirm-target-path operator-asserted`, `--output-dir`, `--run-dir`, and one
   or more `--capture-root` and `--model-visible-root` arguments. Sampling fields
   remain explicit null/unverified; version/tags do not prove active settings.
3. Retain the exclusive `runtime-receipt.json`, `observation.json`, and any failure
   marker. They contain bounded identities/hashes, never raw responses, secrets
   or the rubric. Failure receipts keep a closed diagnostic code (for example,
   timeout versus identity mismatch) and only a short safe observed version token
   or selected hexadecimal model digest; arbitrary exception text is omitted. Never overwrite or delete partial output to retry. The helper pins the output
   directory and rechecks paths around writes; a changed boundary refuses further
   receipts and preserves the original attempt. These checks do not provide an
   atomic filesystem capability against an arbitrary privileged racing writer.
4. Only an explicitly selected `--prepare-only --rubric /absolute/external/path`
   may invoke the existing Cargo preparation entrypoint. This performs local
   capture/state mutation and queues an analyst, but makes no model call. Rust
   remains authoritative for clean commit/tree, app version, @7 fingerprints,
   protocol hash, captures/requests and `frozen-definition` SHA-256. Publish that
   digest before any separately authorized stage under MT-H4-05. The helper never
   sets analyst/auditor stage or execution opt-in and never dispatches CI.

**Pass:** expected read-only runtime/model observations and immutable external
receipts agree, with host assertion and unknown sampling stated honestly; optional
preparation preserves the existing frozen contract. **Fail:** credentials/oracle
leak, unexpected route/redirect, ambiguity, mismatch, overwrite, hidden execution
or forged host/runtime authority. This is not synthetic/Vendure/native acceptance;
`delivery_pass` remains false and #598 stays open. Offline negatives are tested
without HTTP, Cargo, model calls or provisioning.

## MT-W8B-01 — Exact saved finding identity and reopen (AC-0293, T-0293)

**Procedure: UNEXECUTED; #730 implementation available for verification.** This is a planned host
integration verification, not authorization for a real-provider acceptance run.

1. Using the production coordinator/worker with a scripted bounded provider and
   real temporary file-backed stores, produce two cited findings. Record exact
   result bytes/hash, original ledger and historical citation targets. Read review
   metadata: both are pending at revision 0 and this read writes no review row.
2. Use one finding for follow-up. Close/reopen the store/app, remove finished Job
   rows through supported cleanup and reopen the task. Only the chosen finding
   retains revision 1/decision/time; original result bytes/citations match exactly.
3. In a second genuine run also containing finding-1, mix task/result/finding/
   subject fields. Each mismatch fails without writes. Exercise changed valid
   saved requested model/endpoint/protocol/scope/definition and reported-response
   label (including absent); subject changes follow saved identity, not equal prose.
4. Change current runtime/catalog availability after completion: the original
   subject and historical decision remain readable. Corrupt saved result/ledger/
   invocation/citation bindings in isolated negative fixtures: normal reads fail
   closed, and no old subject applies to changed content. Verify the independent
   literal subject serialization fixture as part of implementation tests.

**Pass:** exact historical association survives restart/cleanup without identity
substitution or body changes. **Fail:** result-local IDs, today's configuration or
corrupt evidence can select/redefine another historical decision. Record fixtures,
commands and actual outcomes on #730 before marking executed.

## MT-W8B-02 — Versioned migration, bounded admission and CAS (AC-0294, T-0294)

**Procedure: UNEXECUTED; #730 implementation available for verification.** Use isolated store copies
and production migration/review helpers; never corrupt user data for this check.

1. Open original pre-coverage and complete version-1 stores, including supported
   existing optional extensions. Compare every historical body/hash/namespace;
   only exact version-2 metadata/review schema appears, with zero review rows.
   Check fresh creation and idempotent version-2 reopen. Unknown versions, extra/
   partial/substituted review objects and malformed/orphan rows fail closed.
2. Through test-only fault points in the production helper, fail after metadata
   rebuild, after review DDL and before commit. A separate reopened connection
   observes the complete original version-1 layout/bytes; no partial upgrade.
3. Synchronize two independent connections before opposite revision-0 decisions.
   Exactly one commits revision 1; loser receives the durable winner. Verify SQL
   through a third reopened connection. Repeated stale request cannot increment;
   a fresh deliberate revision-1 decision reaches revision 2.
4. Force failure after review insert/before commit and exhaust actual task/global
   capacity: no decision/counter/body changes. Exercise checked UTF-8 and row/index
   accounting, oversized/deep/duplicate-key/trailing/unknown-field JSON, invalid
   SQL scalar types, malformed identifiers and signed integer overflow.

**Pass:** atomic exact migration and bounded current-state CAS, with all failures
closed and unchanged. **Fail:** repair/partial migration, lost update, stale retry
success, capacity bypass or changed immutable data. Replace/supplement the manual
mapping with actual fault/race/admission tests when those exist.

## MT-W8B-03 — Honest follow-up UI and result compatibility (AC-0295, T-0295)

**Procedure: UNEXECUTED; #730 implementation available for verification.** Use mock/deferred IPC for
race injection, then separately record any native app observation; a story alone
is not native acceptance.

1. Open a cited result in Investigations. Inspect a historical citation; choose
   each follow-up decision deliberately. Verify qualified controls/status, unchanged
   T3/InferredWeak badge, explanation, revision/time and pending-action disabling.
   Reopen through history; no-findings result has zero review controls.
2. Delay responses across task navigation, same-task reopen, pending metadata read
   and a newer decision. No stale response replaces selection/result/review. Race
   two decisions: show winner on conflict and require a deliberate retry.
3. Commit a decision but drop its acknowledgment. UI does not claim success or
   resubmit automatically; explicit refresh retrieves the committed revision.
4. Fail review metadata reads while the unchanged legacy result API and historical
   citation inspection succeed. Keep readable result and last-known decision with
   unavailable/stale status, never invented pending. Exercise valid near-existing-
   MAX_RESULT_BYTES result before/after review: bytes remain identical. New getter/
   mutation return metadata only under existing response bounds at the admitted
   maximum finding count; introduce no smaller result limit or result wrapper.

**Pass:** honest bounded UI/API behavior independent of review availability.
**Fail:** optimistic/lost-response success, stale overwrite, erased valid result,
unqualified acceptance copy or changed legacy encodings. Record actual story/store
and native evidence separately; update automated mappings only for real tests.

## MT-W8B-04 — Review side effects and historical availability (AC-0296, T-0296)

**Procedure: UNEXECUTED; #730 implementation available for verification.** Scripted host verification
needs no real provider, installed runtime or current-checkout acquisition.

1. Establish confirmed graph facts, a real Gap, recovered context/export and a cited
   saved result. Snapshot graph/export/result/ledger/refs/job/task lifecycle revision
   and model/tool counters. Instrument provider and current-source acquisition entry
   points. Read reviews and record both decision directions.
2. Compare all snapshots byte-for-byte: only review association changes; zero
   provider/source-acquisition calls. No worker event/status change, graph proposal,
   prompt injection or execution path appears. Rejection keeps original citations.
3. With structurally valid saved bindings, make retained source unavailable/
   forgotten/corrupt using isolated supported fixtures. The historical decision
   remains; the existing inspector distinguishes availability/invalid/operational
   observations without reading current checkout. Corrupt saved result/ledger/
   citation binding itself disables review rather than repairing it.
4. Verify UI/spec wording does not present accepted follow-up as verified evidence,
   curated activation, execution authorization or #598 acceptance. Any later packet
   consumer remains responsible for fresh independent evidence validation.

**Automated evidence:** `finding_review_scripted_worker_preserves_graph_export_and_source_boundaries`
executes the production worker against a scripted provider and real temporary
stores. It establishes confirmed facts, a Gap and a retained citation; compares
actual graph and export bytes plus non-review storage across both decisions; and
observes provider, retained-source and registered-checkout access at their real
entry points. An explicit historical inspection after checkout replacement still
returns the original capture. This covers steps 1–2 and the no-current-checkout
boundary. `finding_review_survives_confirmed_forget_and_retained_object_failures`
covers step 3 with production preview/confirmation/forget and isolated missing or
corrupt retained objects. Saved decisions/bindings survive reopen and subsequent
CAS changes while inspection reports unavailable/invalid without checkout fallback.
These host tests do not establish native UI observation.

**Pass:** metadata-only human association and independent historical availability.
**Fail:** changed source/graph/export/counters, provider invocation, fallback or
promoted authority. This procedure does not pass W8b/H3/H4/H5 or real-provider gates.

## MT-W8A-01 — Qualified read-session and retained evidence

**Status: PARTIAL, scoped evidence recorded.** AC-0288; #718.
See [native acceptance](acceptance/read-only-mcp-native.md) for observed controlled
acquisition, paging, guarded retention and the dated `a7915641` native
`store_capacity` omission/retained/unrecorded controls. The positive omission
branch is observed; historical and current fixture scopes remain separately
attributed, without a blanket full-procedure claim. Use a disposable,
prepopulated fixture profile, with separately authorized fixture setup. No
provider calls.

1. Record app/build, fixture graph and retained-capture identities. Open a session
   in each export mode. Record original graph ID, filtered projection ID, producer
   coverage, registration basis and explicit H3-unavailable capability.
2. Discover a known confirmed fact, inferred facts and a rejected business rule.
   Compare against official selected compilation: rejected rule adjacency is
   absent, weak facts obey export mode, and missing evidence remains qualified.
3. Page discovery and a produced artifact; verify context, content identity,
   UTF-8 boundaries and explicit limits. Replay with a changed query/context or
   wrong artifact content ID and require rejection, never silent restart.
4. Under controlled fixture changes, race acquisition with registration and
   rejection changes. Require stale/unavailable outcomes when the bracket changes.
   A coverage change with unchanged graph must invalidate the old continuation.
5. Follow a real retained receipt/range using the original graph identity. Change
   graph binding/registration, or forget the fixture capture under authorized
   fixture control, and require no returned replacement/current-checkout text.
   Exercise retention cleanup racing a span read and record the guarded outcome.
6. Record exact returned limitations and failures. **Pass only if all steps have
   observed evidence;** foundation tests alone do not complete this procedure.

## MT-W8A-02 — Installed relay, native boundary and lifecycle

**Status: PARTIAL, isolated ARM macOS evidence recorded.** AC-0289; #718.
See [native acceptance](acceptance/read-only-mcp-native.md); different-user peer
and production/platform qualification remain unestablished. Repeat natively on every advertised
platform with the actual packaged app and console relay.

1. Record signed package/build and relay path. Verify disabled-by-default service,
   explicit activation and private endpoint/record permissions and peer checks.
2. Connect with an independent stdio MCP client. Verify authentication precedes
   initialization/read dispatch, exact supported protocol negotiation, closed
   tool schemas, correlated IDs and response-free notifications.
3. Test invalid/expired credentials, malformed/oversized frames, duplicate IDs,
   unknown tools and mutation/task requests. Require bounded failure with no
   credential or source leakage to diagnostics and no non-MCP stdout output.
4. Exercise configured session, cache, concurrency and idle/authentication limits.
   Record actual configured values and admitted/denied work; do not infer total
   resident-memory bounds from serialized frame or raw graph byte limits.
5. Disable/re-enable and restart the fixture app; old credentials/connections
   must fail. Close the app with relay stdin open: relay exits. Close client stdin
   after a request: final response drains. App unavailable must not launch it.
6. Verify no relay store access, recovery, ingestion or provider invocation, using
   process/dependency inspection and fixture audit evidence. Verify installation,
   configuration and removal instructions. Record unsupported platforms as not
   passed, never as an equivalent Unix fixture result.

## MT-W8A-03 — Independent client and progressive disclosure

**Status: Scoped ARM fixture observations recorded.** AC-0290; #718.
See [native acceptance](acceptance/read-only-mcp-native.md) for exact client,
builds, transcripts, measurements and limitations. No model/provider run is required or
implicitly authorized by this procedure.

1. With read access disabled, use Show packaged client skill beside client
   configuration. Record the app version and verified skill location; check the
   entrypoint and all reference files against the built revision. On a disposable
   bundle copy, remove or modify a packaged reference and confirm explicit
   unavailability without service activation or client settings changes. Restore
   the fixture before continuing. Record independent client/version, MCP protocol,
   packaged app/relay build and fixed fixture basis. Follow the shipped skill from capability discovery to a
   narrowly scoped query, official artifact page and exact retained source span.
2. Save a redacted transcript with schemas, request IDs, qualified responses,
   citations and explicit limited/stale/unavailable outcomes. Verify retrieved
   instruction-like text remains quoted evidence and cannot dispatch actions.
3. Compare disclosed serialized UTF-8 bytes, including tool schemas and protocol
   envelopes, against the same fixture's complete selected artifact. State the
   baseline and all overhead. Report tokenizer/version if measured; otherwise
   mark token counts unknown rather than converting bytes to asserted tokens.
4. Record uncovered cases separately. Byte savings are not evidence of model
   answer quality, completed investigation acceptance or activated H3 curation.
