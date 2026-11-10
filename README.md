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

As of 0.67 the deterministic foundation is built: the desktop recovery engine,
language and infrastructure adapters, graph and flow views, provenance, the
specification compiler and its exports, a read-only MCP surface, and bounded
specialist investigations that answer cited questions.

Not built yet: agents do not write document sections, accepted proposals do not
reach the exports, and documents are not updated when the code drifts. The plan
for those stages is [ROADMAP §10](docs/ROADMAP.md#10-realignment-2026-11-09);
the direction is [ADR-0056](docs/adr/ADR-0056-documentation-reconstruction-pipeline.md).
Planned capabilities have separate delivery gates and are not shipped until
those pass.

```sh
# prerequisites: Node (see .nvmrc) and Rust via rustup — https://rustup.rs
# (rust-toolchain.toml pins the exact toolchain; rustup picks it up automatically)
npm install && npm --prefix ui install
npm run tauri dev   # first run compiles the Rust workspace — takes a few minutes
```

## Documentation map

| Document | Purpose |
|---|---|
| [docs/VISION.md](docs/VISION.md) | Product vision: the end state and its elements V1–V13 |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Roadmap: workstreams, phases with demos, first issues |
| [docs/SPEC-00_master.md](docs/SPEC-00_master.md) | Master specification — single source of truth |
| [docs/SPEC-01_context-hub.md](docs/SPEC-01_context-hub.md) | Domain context, MCP/ACP architecture, and delivery gates |
| [docs/SPEC-02_rule-evidence.md](docs/SPEC-02_rule-evidence.md) | Scoped rule evidence, redaction, and interpretation boundaries |
| [docs/cartograph_project_brief.md](docs/cartograph_project_brief.md) | Short project brief |
| [docs/user_stories.md](docs/user_stories.md) | User stories + acceptance criteria (US/AC schema) |
| [docs/US-TM.md](docs/US-TM.md) | Traceability matrix: US ↔ AC ↔ crate ↔ milestone ↔ ADR ↔ test |
| [docs/adr/](docs/adr/) | Architecture decision records |
| [docs/design/production-ui/](docs/design/production-ui/) | Production UI design handoff (authoritative tokens + screenshots) |
| [docs/design/](docs/design/) | Earlier design tokens + Stitch UI mockups |

Work is tracked in [GitHub Issues + Project](https://github.com/true-line/cartograph/issues);
`docs/archive/tracker.csv` is the pre-import snapshot and is not maintained.

## Stack (decided — see ADRs)

Tauri 2 · Rust core · React + TypeScript + Vite UI · tree-sitter ·
SQLite/WAL graph store with recursive-CTE traversal (ADR-0008) + state spine ·
Ollama local-first LLM with pluggable cloud providers (opt-in egress).

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
