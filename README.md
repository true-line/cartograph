# Cartograph

**Reconstructs the documentation an application should have had from day one.**
Drag in an app, or as much of its code as can be gathered. Cartograph traces the
infrastructure, code, UI and existing documentation into a provenance-tagged
graph. A team of agents then works backwards from those facts to write the
ADRs, the product specification and the rest of an enterprise document set,
and the documents are updated as the code drifts. Engineers use the result to
maintain, rebuild or port the system. Cartograph itself writes no target code.

A project that is already well documented uses the same graph as a **context
hub**: engineers and agents ask questions and get cited answers.

The core thesis is a **four-tier escalation ladder**: every fact is produced by
the lowest tier that can establish it, and carries provenance + a confidence
tier. The engine prefers an explicit **Gap** over an unsupported assertion.

| Tier | Method | Confidence |
|---|---|---|
| T0 Deterministic | Static parse (tree-sitter), IaC/HCL graph, framework adapters | Confirmed |
| T1 Dynamic | Observed evidence (Terraform state, OTel traces, test runs) | Confirmed (observed) |
| T2 Semantic | Local embeddings, similarity/contract matching | InferredStrong |
| T3 Agentic | Bounded LLM agents proposing links with cited evidence | InferredWeak |

The specification compiler produces flow dossiers, decision records, and gap and
drift registers. Domain recovery and complete feature/acceptance-criteria recovery
still need validation; the presence of an artifact is not proof of completeness.

## Status

The latest release is **0.81.0** (2026-11-25). v1.0.0 is not released yet; its
criteria are in [ROADMAP §11](docs/ROADMAP.md#11-v100-scope-2026-11-17).

What the app does today:

- **Recover a system** from a GitHub repository, a local folder or a system
  manifest (several repositories as one system): Connect → Preflight →
  Recover. The deterministic tier reads TypeScript, Python, Go, Rust, Java and
  Kotlin, framework adapters and infrastructure as code, and calls no model.
- **Explore it**: the Atlas topology graph, the Flow Inspector, C4
  architecture views (context, containers, components), the Gaps & Drift
  register and Provenance & Eval. Every fact opens its evidence.
- **Read and curate the recovered documents** in the Spec Workbench: the
  product specification, flow dossiers, ADRs, endpoint inventory and
  permissions, pipelines, configuration keys, external services,
  environments, the gap and drift registers and the fact index, exported as
  one handoff bundle.
- **Ask agents**: specialist investigations and a topic expert per recovered
  area answer with citations; saved conversations are in Agent Chats. A
  Section Writer drafts a section of the product specification, a person
  accepts, rejects or returns it, and an accepted section is marked stale
  when a fact it cites changes and can be re-drafted. Agent output always
  stays T3 / InferredWeak and is shown only after a person accepts it.
- **Serve the graph to other agents** through a read-only MCP surface and
  reviewed agent work packets.

Not done yet: agents drafting the other catalogued documents (D2 to D10) and
the content-acceptance pilot for drafted sections
([#1748](https://github.com/true-line/cartograph/issues/1748)). The direction
is [ADR-0056](docs/adr/ADR-0056-documentation-reconstruction-pipeline.md);
the document set is the [document catalogue](docs/DOCUMENT-CATALOGUE.md).

### Install

Download from the [latest release](https://github.com/true-line/cartograph/releases/latest):

| Platform | File |
|---|---|
| macOS (Intel and Apple Silicon) | `Cartograph_<version>_universal_signed.dmg` |
| Windows x64 | `Cartograph_<version>_x64_signed-setup.exe` |
| Windows ARM64 | `Cartograph_<version>_arm64_signed-setup.exe` |

All three are signed. [First run](docs/help/first-run.md) covers install,
provider setup and the first ingest.

### Build from source

```sh
# prerequisites: Node (see .nvmrc) and Rust via rustup — https://rustup.rs
# (rust-toolchain.toml pins the exact toolchain; rustup picks it up automatically)
npm install && npm --prefix ui install
npm run tauri dev   # first run compiles the Rust workspace — takes a few minutes
```

## Documentation map

| Document | Purpose |
|---|---|
| [docs/design/product-guide.md](docs/design/product-guide.md) | Product guide for design: users, screens, journeys and the rules the UI must keep |
| [docs/help/first-run.md](docs/help/first-run.md) | First run: install the build, set up a provider, first ingest (also in the app under Help) |
| [docs/help/](docs/help/) | In-app help, one page per surface |
| [docs/VISION.md](docs/VISION.md) | Product vision: the end state and its elements V1–V13 |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Roadmap: workstreams, phases with demos, first issues |
| [docs/SPEC-00_master.md](docs/SPEC-00_master.md) | Master specification — single source of truth |
| [docs/SPEC-01_context-hub.md](docs/SPEC-01_context-hub.md) | Domain context, MCP/ACP architecture, and delivery gates |
| [docs/SPEC-02_rule-evidence.md](docs/SPEC-02_rule-evidence.md) | Scoped rule evidence, redaction, and interpretation boundaries |
| [docs/cartograph_project_brief.md](docs/cartograph_project_brief.md) | Short project brief |
| [docs/DOCUMENT-CATALOGUE.md](docs/DOCUMENT-CATALOGUE.md) | Catalogue of the documents Cartograph produces (version 1.0, owner-approved; #1747, #1856) |
| [docs/user_stories.md](docs/user_stories.md) | User stories + acceptance criteria (US/AC schema) |
| [docs/US-TM.md](docs/US-TM.md) | Traceability matrix: US ↔ AC ↔ crate ↔ milestone ↔ ADR ↔ test |
| [docs/adr/](docs/adr/) | Architecture decision records |
| [docs/design/production-ui/](docs/design/production-ui/) | Original production UI handoff: tokens and prototype screenshots (predates Agent Chats, Investigations and Help) |
| [docs/design/](docs/design/) | Earlier design tokens + Stitch UI mockups |

Work is tracked in [GitHub Issues + Project](https://github.com/true-line/cartograph/issues);
`docs/archive/tracker.csv` is the pre-import snapshot and is not maintained.

## Stack (decided — see ADRs)

Tauri 2 · Rust core · React + TypeScript + Vite UI · tree-sitter ·
SQLite/WAL graph store with recursive-CTE traversal (ADR-0008) + state spine ·
Ollama local-first LLM with pluggable cloud providers (opt-in egress).
Builds for macOS (universal) and Windows (x64, ARM64).

## Contributing

This repo runs a strict, agent-first SDLC: every change lands through a gated
PR traceable to a user story / acceptance criterion. See
[CONTRIBUTING.md](CONTRIBUTING.md) and `AGENTS.md` for the workflow.

### Agent skills

Repo-local skills for coding agents live in [`.agents/skills/`](.agents/skills/)
— the tool-agnostic home. Agents whose harness does not auto-discover
skills should read the relevant `SKILL.md` directly before starting the task
it covers.

- [`add-language-adapter`](.agents/skills/add-language-adapter/SKILL.md) —
  scaffold language-adapter coverage (a sandboxed WASM plugin, or an extension
  to a compiled-in `adapters-lang-*` crate). It is the short version of the
  authoritative [adapter authoring guide](docs/adapters/AUTHORING_GUIDE.md)
  (ADR-0017).

- [`read-cartograph-context`](.agents/skills/read-cartograph-context/SKILL.md) —
  retrieve recovered context, official artifacts and retained evidence through
  an existing read-only MCP connection, with progressive disclosure.

## License

Cartograph is **source-available, not open source**. It is licensed under the
[PolyForm Noncommercial License 1.0.0](LICENSE.md): free to use, modify, and
share for any **noncommercial** purpose (personal, research, education, and
nonprofit/government use). **Commercial use requires a separate license** — open
an issue or contact the maintainers at zts1.com.

Third-party dependencies retain their own licenses; see
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) (generated by
`node scripts/generate-notices.mjs`). License compliance is enforced in CI by
[`cargo-deny`](deny.toml).
