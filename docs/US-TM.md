# US-TM.md — Traceability Matrix (Cartograph)

Binds: **US ↔ AC ↔ Crate(module) ↔ Milestone ↔ Flow ↔ ADR ↔ Test**.

| US | AC range | Crate(s) | Milestone | Flow | ADR | Tests |
|----|----------|----------|-----------|------|-----|-------|
| US-0001 | AC-0001..0003, AC-0049..0050, AC-0076..0078, AC-0085, AC-0094, AC-0195..0198, AC-0202..0203, AC-0224, AC-0215 | ingest, source-walk, core-graph, app, ui | M0–M3 | — | ADR-0001, ADR-0005, ADR-0009 | T-0001..0003, T-0049..0050, T-0076..0078, T-0085, T-0094, T-0195..0198, T-0202..0203, T-0224, T-0215 |
| US-0002 | AC-0004..0006, AC-0053..0054, AC-0079..0080, AC-0095, AC-0098..0100, AC-0192, AC-0199..0201, AC-0206, AC-0207, AC-0209, AC-0214, AC-0216, AC-0212, AC-0220, AC-0213, AC-0226, AC-0230, AC-0284, AC-0298, AC-0300, AC-0310, AC-0338, AC-0339, AC-0340, AC-0341, AC-0360, AC-0363, AC-0364, AC-0367, AC-0399, AC-0407, AC-0410, AC-0415, AC-0421, AC-0424, AC-0425, AC-0428, AC-0431, AC-0433, AC-0434, AC-0435, AC-0437, AC-0438, AC-0441, AC-0442, AC-0443, AC-0444, AC-0451, AC-0458, AC-0456..0457, AC-0459, AC-0462, AC-0469, AC-0467, AC-0468, AC-0474, AC-0475 | adapters-lang-ts, adapters-lang-python, adapters-lang-go, adapters-lang-rust, adapters-lang-java, adapters-lang-kotlin, adapters-fw, core-graph, core-prov, ingest, spec, app, ui | M1, M10 | — | ADR-0003, ADR-0006, ADR-0033, ADR-0035 | T-0004..0006, T-0053..0054, T-0079..0080, T-0095, T-0098..0100, T-0192, T-0199..0201, T-0206, T-0207, T-0209, T-0214, T-0216, T-0212, T-0220, T-0213, T-0226, T-0230, T-0284, T-0298, T-0300, T-0310, T-0338, T-0339, T-0340, T-0341, T-0360, T-0363, T-0364, T-0367, T-0399, T-0407, T-0410, T-0415, T-0421, T-0424, T-0425, T-0428, T-0431, T-0433, T-0434, T-0435, T-0437, T-0438, T-0441, T-0442, T-0443, T-0444, T-0451, T-0458, T-0456..0457, T-0459, T-0462, T-0469, T-0467, T-0468, T-0474, T-0475 |
| US-0003 | AC-0007..0009, AC-0043..0048, AC-0051..0052, AC-0225, AC-0316..0318, AC-0331, AC-0337, AC-0365, AC-0376 | adapters-lang-ts, iac, adapters-lang-go, dynamic, spec, app, adapters-fw, ingest, ui | M2, M6 | — | ADR-0003, ADR-0034 | T-0007..0009, T-0043..0048, T-0051..0052, T-0225, T-0316..0318, T-0331, T-0337, T-0365, T-0376 |
| US-0004 | AC-0010..0012, AC-0256 | events, dynamic, adapters-fw, adapters-lang-ts, flowtracer, app | M3, M5–M6 | F-* | ADR-0002 | T-0010..0012, T-0256 |
| US-0005 | AC-0013..0014, AC-0452..0453 | adapters-lang-ts(tsx), adapters-fw, events | M4 | F-* | ADR-0003 | T-0013..0014, T-0452..0453 |
| US-0006 | AC-0015..0017,AC-0083,AC-0219,AC-0257,AC-0330,AC-0332,AC-0333,AC-0334,AC-0335,AC-0336 | flowtracer, ingest | M3–M5 | F-* | ADR-0002, ADR-0032 | T-0015..0017,T-0083,T-0219,T-0257,T-0330,T-0332,T-0333,T-0334,T-0335,T-0336 |
| US-0007 | AC-0018..0020,AC-0061 | core-prov, agents, ui | M0, M8 | — | ADR-0002, ADR-0006 | T-0018..0020,T-0061 |
| US-0008 | AC-0021..0022, AC-0269, AC-0285 | adapters-lang-ts, iac, core-graph, semantic, llm, app | M7 | F-* | ADR-0002, ADR-0004, ADR-0010, ADR-0037 | T-0021..0022, T-0269, T-0285 |
| US-0009 | AC-0023..0025,AC-0055..0056,AC-0063,AC-0087,AC-0089,AC-0231 | agents, llm, ingest, app, ui | M8 | — | ADR-0004 | T-0023..0025,T-0055..0056,T-0063,T-0087,T-0089,T-0231 |
| US-0010 | AC-0026..0028,AC-0062,AC-0064,AC-0081,AC-0086,AC-0366 | core-graph, app, ui | M9 | — | ADR-0001 | T-0026..0028,T-0062,T-0064,T-0081,T-0086,T-0366 |
| US-0011 | AC-0029..0031, AC-0065..0066, AC-0084 | app, flowtracer, ui | M9 | F-* | ADR-0002 | T-0029..0031, T-0065..0066, T-0084 |
| US-0012 | AC-0032..0035,AC-0057..0058,AC-0067,AC-0210,AC-0218,AC-0221,AC-0258,AC-0297,AC-0358,AC-0359 | core-prov, spec, agents, app, flowtracer, ui | M9–M10, #993, #994 | — | ADR-0002, ADR-0011 | T-0032..0035,T-0057..0058,T-0067,T-0210,T-0218,T-0221,T-0258,T-0297,T-0358,T-0359 |
| US-0013 | AC-0036..0038,AC-0059,AC-0082,AC-0088,AC-0259,AC-0313 | ingest, spec, app, ui | M9 | F-* | ADR-0002, ADR-0012 | T-0036..0038,T-0059,T-0082,T-0088,T-0259,T-0313 |
| US-0014 | AC-0039..0040,AC-0060,AC-0205,AC-0208,AC-0217,AC-0223,AC-0423 | source-walk, adapters-lang-*, iac, ingest, core-graph, core-prov, app, ui | M10 | — | ADR-0006, ADR-0014, ADR-0018, ADR-0030 | T-0039..0040,T-0060,T-0205,T-0208,T-0217,T-0223,T-0423 |
| US-0015 | AC-0041..0042, AC-0380, AC-0383, AC-0387, AC-0395, AC-0446, AC-0448 | iac, spec, ui, adapters-lang-python | M9 | — | ADR-0003, ADR-0013 | T-0041..0042, T-0380, T-0383, T-0387, T-0395, T-0446, T-0448 |
| US-0016 | AC-0071..0075 | adapters-lang-ts, adapters-fw, events, spec, app, ui | post-M10 | WebExtension flows | ADR-0003, ADR-0013 | T-0071..0075 |
| US-0017 | AC-0068..0070, AC-0093 | adapters-*, ingest, app, ui | post-M10 | — | ADR-0003, ADR-0017 | T-0068..0070, T-0093 |
| US-0018 | AC-0090..0092, AC-0222 | app, ingest, ui, scripts | post-M10 | — | ADR-0002, ADR-0018 | T-0090..0092, T-0222 |
| US-0019 | AC-0096..0097 | ingest (toolchain config facts; keys-only env files including `*.env`/`env.example` templates, quote-aware key parsing, span-free `CONFIGURED_IN` links from env-keyed observed resources, #734), core-graph, spec (topology draws `CONFIGURED_IN` env files), app, ui | post-M10 | — | ADR-0003 | T-0096..0097 |
| US-0020 | AC-0101..0105, AC-0137..0139, AC-0232..0233, AC-0302..0303, AC-0460 | context-hub, core-graph, app | H1 | — | ADR-0008, ADR-0019, ADR-0047 | T-0101..0105, T-0137..0139, T-0232..0233, T-0302..0303, T-0460 |
| US-0021 | AC-0106..0109 | context-hub, agents, spec, app, ui | H2–H3 | — | ADR-0019 | T-0106..0109 |
| US-0022 | AC-0110..0114 | context-hub, agents, app, ui | H4–H5 | — | ADR-0019 | T-0110..0114 |
| US-0023 | AC-0115..0118 | context-hub, agents, app, ui | H6–H7 | — | ADR-0019 | T-0115..0118 |
| US-0024 | AC-0119..0125 | adapters-lang-ts (lexical handlers and fail-closed syntax), core-graph, core-prov, core-redact, spec, context-hub, app | H2 | — | ADR-0003, ADR-0019, ADR-0020 | T-0119..0125 |
| US-0025 | AC-0126..0131 | agents (bounded replay admission), core-graph, app, ui | H3 | — | ADR-0019, ADR-0021 | T-0126..0131 |

| US-0026 | AC-0132..0136 | source-capture (durable read policy), spec | H3 | — | ADR-0022 | T-0132..0136 |

| US-0027 | AC-0140..0147 | app, ingest, core-graph, ui | H3 | — | ADR-0019, ADR-0023 | T-0140..0147 |

| US-0028 | AC-0148..0155, AC-0280, AC-0282, AC-0283, AC-0292, AC-0299, AC-0301, AC-0314, AC-0319, AC-0373 | adapters-lang-ts, source-capture, core-graph, ingest, app (retained path identity and no-follow revalidation), ui | H3 | — | ADR-0024, ADR-0030, ADR-0039, ADR-0046 | T-0148..0155, T-0280, T-0282, T-0283, T-0292, T-0299, T-0301, T-0314, T-0319, T-0373 |
| US-0029 | AC-0156..0163 | app (durable Windows NTFS job storage), ui | post-M10 | — | ADR-0025 | T-0156..0163 |

| US-0030 | AC-0164..0171 | adapters-lang-ts, core-graph, spec, context-hub, app, ui | H2 | — | ADR-0020, ADR-0024, ADR-0026 | T-0164..0171 |

| US-0031 | AC-0172..0179 | agents, core-graph, app, ui | H3 | — | ADR-0021, ADR-0024, ADR-0027 | T-0172..0179 |

| US-0032 | AC-0180..0191, AC-0204, AC-0234..0236, AC-0260..0262, AC-0267, AC-0279, AC-0281, AC-0286, AC-0287, AC-0342, AC-0368, AC-0370, AC-0372, AC-0374..0375, AC-0377..0379, AC-0381..0382, AC-0384..0386, AC-0388..0391, AC-0392..0394, AC-0396..0398, AC-0400..0406, AC-0408..0409, AC-0411..0414, AC-0416..0420, AC-0422, AC-0429..0430, AC-0432, AC-0436, AC-0439..0440, AC-0445, AC-0447, AC-0449, AC-0450, AC-0455, AC-0461, AC-0464, AC-0465, AC-0466, AC-0463, AC-0472, AC-0473, AC-0480, AC-0481, AC-0483, AC-0482, AC-0484 | agents (bounded durable record round-trip serialization, immutable @1/@2 history, compact @3 disclosure, explicit @4 turn progress and available specialist replacement, compiled Spec Writer and Gap Triager definitions with exact completion families and admitted Spec Writer @2 cited narrative), llm (bounded transport @2), core-graph, context-hub, app (complete query-page budgets, conditional next-prompt safety within existing limits and captured-profile analyst preparation with partial/refused evidence distinct from full acceptance (AC-0287), explicit manual captured Local-provider analyst execution/reopen with independent frozen identity and one-attempt protection (AC-0429), content-free ordinary investigation and typed action-decode stage/class, provider-code and limit-origin refusal diagnostics (AC-0430) and one bounded finish-only source-replay correction with cancellation-safe settlement (AC-0184), bounded persisted chat reads, bounded saved-provider activity projection, conversation-scoped task history), ui (persisted task-chat transcript, versioned specialist activity with saved provider attribution, exact-task opening and shared bounded status-count rollup, Spec Writer and Gap Triager selection and saved result, quick completion dispatch with overlay-local choices, bounded saved-result notices with a four-corner local preference, dedicated Agent Chats destination with up to four saved-task transcript docks, close/reopen and exact-result navigation, loaded exchanges grouped by persisted conversation with parent links and stable reply drafts, saved-result notice routing to exact chat or findings, confirmed quick-dispatch routing to its exact saved chat, parent-bound child-task follow-up dispatch, focused saved-answer history and final narrative search) | H4 | — | ADR-0019, ADR-0024, ADR-0025, ADR-0028, ADR-0040, ADR-0041, ADR-0042 | T-0180..0191, T-0204, T-0234..0236, T-0260..0262, T-0267, T-0279, T-0281, T-0286..0287, T-0342, T-0368, T-0370, T-0372, T-0374..0375, T-0377..0379, T-0381..0382, T-0384..0386, T-0388..0394, T-0396..0398, T-0400..0406, T-0408..0409, T-0411..0414, T-0416..0420, T-0422, T-0429..0430, T-0432, T-0436, T-0439..0440, T-0445, T-0447, T-0449, T-0450, T-0455, T-0461, T-0464, T-0465, T-0466, T-0463, T-0472, T-0473, T-0480, T-0481, T-0483, T-0482, T-0484 |
| US-0033 | AC-0237..0239, AC-0264..0266, AC-0268 | ingest, context-hub, agents, app, ui, adapters-lang-ts | P1 (ROADMAP) | — | ADR-0036 | T-0237..0239, T-0264..0266, T-0268 |
| US-0034 | AC-0240..0241, AC-0315 | events, ingest, app, adapters-lang-ts, adapters-lang-kotlin, adapters-lang-java | P1 (ROADMAP) | F-* | ADR-0002, ADR-0036 | T-0240..0241, T-0315 |
| US-0035 | AC-0242..0245 | spec, core-prov, core-graph, app, ui | P1 (ROADMAP) | — | ADR-0036 | T-0242..0245 |
| US-0036 | AC-0246..0247 | spec, app, ui | P2 (ROADMAP) | F-* | ADR-0036 | T-0246..0247 |
| US-0037 | AC-0248..0252 | agents, spec, app, ui | P3 (ROADMAP) | — | ADR-0019, ADR-0036 | T-0248..0252 |
| US-0038 | AC-0253..0255, AC-0263, AC-0361, AC-0362, AC-0369, AC-0371, AC-0426, AC-0427 | ingest, adapters-fw, adapters-lang-go, dynamic, adapters-plugin-host, app, ui | P1 (ROADMAP) | — | ADR-0017, ADR-0036 | T-0253..0255, T-0263, T-0361, T-0362, T-0369, T-0371, T-0426, T-0427 |
| US-0039 | AC-0270 | semantic, app | P1 (#257) | — | ADR-0036 | T-0270 |
| US-0040 | AC-0271..0278 | semantic, app, ui | P3 | — | ADR-0037, ADR-0038 | T-0271..0278 |
| US-0041 | AC-0288..0290, AC-0304..0309, AC-0311..0312 | core-graph, spec, context-hub, mcp-wire, mcp-local, mcp-relay, app, ui | W8a (#718, #732 packaged client skill, #840 lazy read acquisition) | — | ADR-0044 | T-0288..0290, T-0304..0309, T-0311..0312 |

| US-0042 | AC-0293..0296 | agents, app, ui | W8b / H4 dependency (#730) | — | — | T-0293..0296 |

| US-0043 | AC-0320..0329 | agents (durable client identity, task attribution, runtime grant primitive, record decoder scratch component, bounded archive comparison and ledger/archive semantic-validation and inventory-preparation DTO workspace), app (actual-Service synthetic-provider relay acceptance, owner-only task activation and observed grant with explicit Settings controls, authenticated service-to-worker task session handoff, durable source settlement writer ownership, source-capacity replay map/set ownership, shared receipt decode workspace used by evidence reservation, host activation lifecycle, grant-guarded principal-scoped storage reads/cancel and caller-intent-derived start admission and early retry lookup through the existing coordinator, pre-admission and detached completion source-discovery limits with typed cap refusals, detached reservation count and lifetime, initial frozen-context, accepted read growth and prepared-inventory ownership across execution/replay admission; host query admission includes canonical-path guard ownership and full preparation bounds; retained-source admission includes base/candidate ledgers, selected facts, guards/receipt, exact evidence and saved task details outside the execution-phase maximum, with shared capture manifest/file/span read workspace in the execution phase; guarded source preparation/revalidation includes preliminary guard overlap, with enlarged source-candidate validation before manifest refusal; durable source snapshot ownership includes nested materialization and the returned snapshot across input construction; write settlement scratch remains separate; transport integration remains planned), ui (owner credential rotation), llm (checked bounded HTTP payload and firewall ownership components; complete worker admission remains planned), context-hub (scope reference selection, checked core/qualified query workspace and coverage batch validation workspace; host preparation/worker composition remain planned), adapters-lang-ts and core-graph (receipt fact-match and rule-validation workspace), mcp-wire (closed task argument envelopes), mcp-local, mcp-relay | H4 | — | ADR-0028, ADR-0044, ADR-0048 | T-0320..0329 |
| US-0044 | AC-0343..0348 | adapters-lang-python, adapters-lang-ts (GraphQL resolvers, Vitest/Jest tests), spec (`test_arrangements.md`, test and contract drift in `drift_register.md`, flow dossier test references), app, ui | W10a (#946), W10b (#953), W10c (#968), W10d (#965) | — | — | T-0343..0348 |
| US-0045 | AC-0349..0357 | adapters-lang-ts (Vue Router screens and navigations), adapters-lang-kotlin (Android Navigation 3 screens and back-stack navigations, declared-type receiver calls, Hilt view-model bindings, Hilt `@Binds` dispatch), adapters-lang-python (SQLAlchemy writes), ingest (coverage matrix), spec (journeys artifact, `hilt_dispatch` assumptions, observed writes inside anchor flows) | W11 (#989, W11a-2, W11a-3, W11b-1, W11c-1, W11c-2, W11c-3, W11d #1066) | — | ADR-0050 | T-0349..0357 |
| ADR-0051 | Layered knowledge model: SQLite deterministic base of record, derived knowledge layer (embeddings ingest, hybrid FTS5 + vector retrieval, knowledge-node producers), artifact evidence basis for targets without source (proposed; owner review required) |

## Coverage assertions
- Every Must-priority US is anchored to an original milestone, post-M10 work, or a SPEC-01 delivery gate; a milestone label alone does not establish completion.
- Every AC has at least one test ID reserved (T-XXXX), to be authored alongside the AC (test-trace mapping standard).
- Integrity rules R-INT-1..5 are covered by US-0007 (AC-0019, AC-0020), US-0006 (AC-0016), US-0012 (AC-0034).

## ADR index
| ADR | Title |
|-----|-------|
| ADR-0001 | Tauri + Rust core + React web UI |
| ADR-0002 | Escalation ladder + provenance-first integrity model |
| ADR-0003 | tree-sitter deterministic extraction + adapter SPI |
| ADR-0004 | Pluggable LLM, local-first, per-tier cloud opt-in |
| ADR-0005 | GitHub App auth + git2 clone + system topology manifest |
| ADR-0006 | Kuzu graph store + SQLite/WAL spine + content-addressed determinism |
| ADR-0007 | Strict agentic SDLC from inception; dogfood v1, expand scope later |
| ADR-0008 | SQLite recursive-CTE as primary graph store (Kuzu archived upstream) |
| ADR-0009 | v1 GitHub auth ladder: env token → gh CLI; GitHub App deferred |
| ADR-0010 | USearch + Ollama semantic staging with paired-eval gating |
| ADR-0011 | Content-addressed full-spec bundle and curation log |
| ADR-0012 | Explicit Markdown ADR directives and derived decision overlays |
| ADR-0013 | Explicit security facts and deterministic finding projection |
| ADR-0014 | Content-addressed delta extraction and graph reconciliation |
| ADR-0015 | Single-source semantic versioning and Changesets release intent |
| ADR-0016 | Fail-closed macOS Developer ID distribution |
| ADR-0017 | Runtime-loadable, AI-authorable adapter plugins (WASM) |
| ADR-0018 | Multi-platform port scope: Windows now, mobile not pursued |
| ADR-0019 | Domain context hub with shared reads and MCP/ACP orchestration |
| ADR-0020 | Source rule evidence and deterministic redaction |
| ADR-0021 | Durable host-owned proposal staging before curated projection |
| ADR-0022 | Immutable source captures before producer verification |
| ADR-0023 | Durable source registration before production capture receipts |
| ADR-0024 | Producer receipts and explicit current source associations |

| ADR-0025 | Execution ownership and attempt-fenced job transitions |

| ADR-0026 | Local definition evidence without value substitution |

| ADR-0027 | Coherent captured agent task basis and immutable staging v2 |

| ADR-0028 | Separate durable investigations from edge-resolution proposals |
| ADR-0029 | Risk-tiered review: owner review for protected paths only |
| ADR-0030 | One shared, `.gitignore`-aware source walk |
| ADR-0031 | Placeholder provenance and proven import boundaries |
| ADR-0032 | Owned execution Gaps make a flow Partial |
| ADR-0033 | Proven-external JVM import ids key on the package |
| ADR-0034 | Explicit references override `.gitignore`; direct reads honor it |
| ADR-0035 | Erasure-level parameter signatures in JVM method identity |
| ADR-0036 | Lifecycle reconstruction as the product direction |

| ADR-0037 | Bounded persistent semantic vectors |

| ADR-0038 | Independent evaluation for semantic families |

| ADR-0039 | Citable TS/JS declaration facts |

| ADR-0040 | Immutable investigation budget and admission policies (proposed) |

| ADR-0041 | Bounded query history with explicit runtime families (proposed) |

| ADR-0042 | Prove continuation capacity before tool admission (proposed) |

| ADR-0046 | Compact receipts, positive absence records and reclaimable retention (proposed; receipt v3, per-fact omission records and forget reclaim implemented) |
| ADR-0047 | Graph store revision (proposed) |

| ADR-0044 | App-owned read-only MCP with a separate local relay (proposed; #718) |

| ADR-0048 | MCP investigation authority and durable client identity (proposed; owner review required) |
| ADR-0050 | Journey identity and tier: T0 navigation facts, T2 journey candidates (proposed; owner review required) |
