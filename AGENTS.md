# Cartograph — agent context

This is the shared context file for all coding agents (and humans acting like
them). It holds the product invariants and the SDLC workflow. There are no tool-specific instruction files; this file is the only agent context.

## What Cartograph is

A local-first desktop app (Tauri 2, Rust core, React UI) that reconstructs the
documentation a system should have had: it traces infra, code, UI and docs
deterministically, agents write the document set from those facts, and the
documents follow the code as it drifts. Engineers rebuild from the result; the
app generates no target code. Well-run projects use it as a context hub. Goal:
`docs/VISION.md`, ADR-0056. Read `docs/SPEC-00_master.md` before product code.

## Orientation

- Layout: Rust workspace per SPEC-00 §8.1 (`crates/*` = analysis engine,
  `src-tauri` = the `app` shell crate, `ui/` = React front end). Not every
  crate under `crates/*` is implemented yet — check the crate's own source
  before assuming a stub.
- When implementing a milestone, work from its exit gate in SPEC-00 §14 and
  the user stories mapped to it in `docs/US-TM.md`.
- SPEC-00 §15 lists four "verify-at-build" claims (Kuzu fit, `hcl-rs`
  coverage, `usearch` bindings, OTel ingest format) — confirm these against
  current reality before relying on them; they may have drifted.

## Product invariants (violating these fails review)

- **Escalation ladder:** every fact is produced by the lowest tier that can
  establish it — T0 deterministic > T1 dynamic > T2 semantic > T3 agentic.
- **R-INT-1** T2/T3 never overwrite or upgrade a T0/T1 fact.
- **R-INT-2** Every node/edge stores tier + confidence; inferred content is
  never indistinguishable from confirmed — in code, UI, or exports.
- **R-INT-3** Agents (T3) are propose-only; no write access to confirmed facts.
- **R-INT-4** A flow with an unresolved hop is emitted partial with an explicit
  Gap node — never silently completed.
- **R-INT-5** `verified-only` export excludes InferredWeak; `best-effort`
  includes it clearly annotated.
- **Non-goals:** no editing of ingested target code (NG1); no automatic code
  regeneration/scaffolding (NG2 — ADR-0019, ADR-0056: rebuild is enabled, not performed);
  no multi-user backend in v1 (NG5).
- **Deterministic tier never calls the LLM.** Local-first; cloud egress is
  per-tier opt-in with explicit consent (fail closed).
- **Determinism:** re-ingesting the same commit under the same source
  registration yields an identical graph (content-hash equality) — a CI
  invariant from M10 on. It is scoped per registration: direct-local and
  `file://` managed sources carry random `local/` registry keys (ADR-0023),
  so no cross-installation equality is claimed for them; only GitHub-backed
  managed sources have stable `owner/name` keys (SPEC-00 §13).

## SDLC workflow (strict, from the first PR)

1. **Work starts from an issue.** GitHub Issues + the Cartograph project board
   are the canonical tracker; milestones M0–M10 mirror SPEC-00 §14. No issue →
   create one first (templates in `.github/ISSUE_TEMPLATE/`).
2. **Branch per change** (`feat/…`, `fix/…`, `chore/…`, `docs/…`); never commit
   to `main` directly.
3. **Spec before code.** If behavior changes, update `docs/user_stories.md`
   (US/AC schema at the top of that file) and the traceability matrix in the
   same PR: new ACs and ADR index rows go in a new fragment file in
   `docs/us-tm.d/` (see its README), not in a shared `docs/US-TM.md` row.
   Architectural decisions get an ADR in `docs/adr/` (format: Status/Date/
   Deciders, Context, Decision, Consequences, Alternatives ≤3).
4. **PR body links its issue** with a closing keyword (`Closes #N`) — merged
   PRs close their issues automatically via
   `.github/workflows/close-linked-issues.yml`.
5. **Gates must pass.** CI runs docs/traceability, Rust, and frontend gates
   (see "Verification before done"). Gates are ratchets — they only get
   stricter.
6. **Traceability is enforced**, not aspirational: `node
   scripts/check-traceability.mjs` validates US ↔ AC ↔ matrix ↔ ADR ↔ test-map
   consistency and runs in CI. Run it before pushing docs changes.
7. **Tests are traced.** Every reserved T-XXXX id has a row in
   `docs/test-map.md` binding it to real tests (`rust:`/`story:`) or a
   procedure in `docs/manual-tests.md`; CI verifies the referenced tests
   exist. A PR that realizes an AC updates its map row in the same PR, and
   test code cites its AC in a comment. Manual procedures run at milestone
   boundaries, results recorded on the milestone's task issue.

## Review and merge policy

Review is risk-tiered ([ADR-0029](docs/adr/ADR-0029-risk-tiered-review.md)).
Paths in `.github/CODEOWNERS` require the owner's approval, and the ruleset
enforces it. Any other PR merges once the Cursor Approval Agent has approved
its current head and every gate is green; the implementing agent merges it.
Request owner review instead (add the `owner-review` label and do not merge)
for critical changes CODEOWNERS cannot see: fact identity or graph-hash
changes, confidence-tier or provenance semantics, data migrations or loss, and
any `priority:must` change to user data. A PR that adds a new agent tool
directory or build entrypoint also adds it to CODEOWNERS. Never add a `*` rule
to CODEOWNERS or remove a protected path without the owner's approval.

## Governed CI lifecycle

Cartograph inherits the execution policy from `qwts/playbook-engineering`
ENG-0004 and the reviewed [CI execution policy](https://raw.githubusercontent.com/qwts/playbook-engineering/main/docs/reference/ci-execution-policy.md).
The policy changes when
the agreed suite runs; it does not reduce the gate list below.

- Draft PR events start no Actions jobs. Before marking a PR ready, run the
  complete local gate list in this file. After pushing the final branch SHA,
  an agent may dispatch **CI** with purpose `exact-sha-preflight` and must wait
  for that exact SHA to succeed before relying on it.
- A ready PR verifies exact-head manual evidence and otherwise runs the complete
  suite. Advanced CodeQL runs on every ready PR run regardless, because the
  ruleset requires its contexts on the PR itself. Ready updates cancel obsolete
  PR runs. Dependabot follows the same lifecycle and review bar.
- The merge queue runs the complete suite on its exact `merge_group` candidate.
  A `main` push with successful evidence for that exact SHA runs only Advanced
  CodeQL plus the short repository-integrity smoke; missing evidence runs the
  complete suite fail-safe. Until the queue is enabled, a passing ready-PR run
  counts as that evidence only when the merged PR's head tree is identical to
  the pushed commit's tree; the Version packages PR never borrows it.
- Manual CI dispatch is limited to exact-SHA preflight, diagnostics, release
  recovery, workflow testing, and explicit reruns. Public-fork workflows are
  never approved or run.
- The immutable CI-policy action validates both `github.actor` and
  `github.triggering_actor`. Repository Actions Policy allows only `qwts`,
  `trueline-chores-dumb[bot]`, `dependabot[bot]`, and active Apps in the governed agent
  roster. Authorization does not imply that a credential must exist.
- Advanced CodeQL is lifecycle-called for Actions, JavaScript/TypeScript, and
  Rust. Default setup must remain disabled after the workflow is enabled.

Required repository settings are manual until governance automation supports
them: activate Actions Policy with the exact actor roster and `merge_group`;
require stable `CI` and retain independent `CodeQL`; switch CodeQL default
setup to Advanced; and require the merge queue with method `MERGE`, grouping
`ALLGREEN`, one candidate build, and one PR per merge. Until the queue is
enabled, the exact-commit `main` fallback protects merge/squash SHA rewrites.

## Versioning and releases

- `package.json` is the canonical application version. The root and UI npm
  lockfiles, workspace `Cargo.toml`, local workspace package entries in
  `Cargo.lock`, and `src-tauri/tauri.conf.json` are synchronized mirrors.
  Never edit a mirror independently: run `npm run version:sync`, and verify
  with `npm run version:check`.
- User-visible behavior/features and breaking changes on the `0.x` line use a
  minor Changeset; fixes use a patch Changeset. After `1.0.0`, breaking changes
  use a major Changeset. Docs, tests, and internal tooling that do not change
  the shipped application may omit one. Run `npm run changeset` to record the
  release note; `CHANGELOG.md` is generated and must not be hand-edited.
- Version application is automation-owned. Do not run `changeset version`,
  edit version mirrors, or create release tags by hand. The version-cut
  workflow keeps a **Version packages** PR current while Changesets are
  pending; its commit is API-written as `trueline-chores-dumb` so it is
  signature-verified under the default-branch ruleset. Merging that reviewed
  PR is the release decision: it creates the
  exact annotated `vX.Y.Z` tag and dispatches the release handoff. A failed
  handoff is recovered by manually dispatching the version-cut workflow; tags
  are immutable and are never moved.
- A Version packages PR receives the normal ready/queue complete suite once;
  version-cut never dispatches a duplicate suite. It opens the PR ready for
  review, so the suite starts with no manual step, and never changes its draft
  state afterwards: an owner who moves the cut to draft keeps it there. An
  unchanged release patch is not force-pushed (#347). Tag and release automation
  require exact-commit CI and reviewed-PR evidence before release-specific work.
- macOS production bundles are universal Intel + Apple Silicon `.app` and `.dmg`
  artifacts. Signing is all-or-nothing across the five repository secrets
  `CSC_LINK`, `CSC_KEY_PASSWORD`, `APPLE_API_KEY`, `APPLE_API_KEY_ID`, and
  `APPLE_API_ISSUER`. With none, packaging emits visibly labeled
  `unsigned-dev` artifacts; any partial set fails. Only signed, notarized,
  Gatekeeper-verified artifacts qualify for a production release.

## Verification before "done"

```sh
node scripts/check-traceability.mjs && node scripts/check-help-mirror.mjs --validate && node scripts/check-workspace-lints.mjs && npm run version:check && npm run test:version
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
npm --prefix ui run lint && npm --prefix ui run typecheck && npm --prefix ui run test && npm --prefix ui run build
```

CI runs the same Rust suite in parallel parts (#1924): the Rust job runs
every package except `app`, `agents` and `spec`, split into three matrix
groups (`adapters` and `ingest` name their packages, `core` runs the rest of
the workspace, so a new crate lands there), "Rust agents & spec
tests" runs `cargo test -p agents -p spec`, and six "Rust app tests" shards run the `app`
test binary through `scripts/rust-test-shard.mjs`, which fails a shard whose
run does not account for exactly the tests it selected. The shards split the
tests by the measured seconds in `scripts/rust-test-weights.json` (the script's
header says how to regenerate it); keep each slow replay scenario its own test,
since one test can't be split across shards. The app crate itself compiles
incrementally from state the "App incremental cache" job saves from main
(`scripts/rustc-app-incremental.sh`), so a shard recompiles only what the
pull request changed. Clippy runs in its
own "Rust clippy" lane.

The Rust lanes keep dependencies in rust-cache and workspace library crates
in sccache (GitHub Actions cache). rust-cache keys on the job id and on the
values of `CARGO_*` and `RUST*` job env vars, so renaming a Rust job or
changing such a variable costs one cold run (~20 minutes) before the lane is
warm again; test binaries are always compiled, as sccache cannot cache them.

The complete CI suite also requires native job ownership tests on Windows:
`cargo test --locked -p job-execution-tests --test ownership`. Every run uses
the GitHub-hosted `windows-latest` runner, a native Windows host, never WSL.
The repository is public, so every CI job runs on GitHub-hosted runners
(`ubuntu-latest`, `windows-latest`); no job a pull request can start runs on a
self-hosted runner, because fork PRs would execute on it. The exceptions are
macOS release packaging (`package.yml`) and the scheduled checks
(`scheduled.yml`), which run on the organisation's self-hosted Macs
(`self-hosted, macOS, ARM64`) and have no pull-request trigger.

Which gates run where:

| Gate | Linux (`ubuntu-latest`) | Windows (`windows-latest`) | macOS (self-hosted, `scheduled.yml`) |
|---|---|---|---|
| Format, clippy, docs and traceability, notices, zizmor, CodeQL, frontend | every PR and merge | — | — |
| `cargo test` (every package; `app` in six shards on Linux, four processes on macOS) | every PR and merge | — | daily; not a PR gate |
| Job ownership tests (`job-execution-tests --test ownership`) | every PR and merge (in the workspace run) | every PR and merge | daily (in the workspace run) |
| Real-repository determinism (AC-0593, MT-1812-01) | — | — | daily, Cartograph at its pinned commit |
| SimpleLogin showcase thresholds (AC-1700) | — | — | weekly, release build |
| Scale showcase thresholds (AC-1700) | — | — | monthly or on demand |
| Changelog versions are tagged (#1762) | daily (`scheduled.yml`) | — | — |

A scheduled lane does not block a PR. It opens or updates one tracking issue
named `Scheduled: …` when it fails and closes it when it passes again; its
result also shows on the main commit it ran on. The Mac lanes share one
concurrency group, so at most one of them builds at a time beside release
packaging.
The harness imports the production job modules and their existing tests without
building Tauri. Local workspace tests run the same harness on the local platform;
only a successful exact-SHA Windows lane establishes its Windows runtime result.
This focused gate does not establish full Windows application support or physical
power-loss durability. It uses the existing governed CI lifecycle and manual
purposes, alongside every gate above.

To see the app itself: `npm run tauri dev` (root). This section is the single
place that lists the gates; CI mirrors it exactly.

**UI components ship with a story.** `ui/` components are presentational
(props in, callbacks out) with a `*.stories.tsx` next to them; interaction
behavior goes in `play` functions (they run as tests via the vitest storybook
project — part of `npm run test`). Stories needing backend data mock the Rust
core with `mockIPC` from `@tauri-apps/api/mocks` (pattern: `App.stories.tsx`).
Storybook itself: `npm run storybook` (from `ui/`).

## Documentation map

- `docs/SPEC-00_master.md` — master spec (single source of truth)
- `docs/SPEC-01_context-hub.md` — staged domain-hub and MCP/ACP extension
- `docs/user_stories.md` / `docs/US-TM.md` — US/AC + traceability matrix
- `docs/adr/` — ADRs for *this app's own* decisions
- `docs/VISION.md` — the product goal and its elements V1–V13; not license to build
- `docs/design/production-ui/` — production UI design handoff (authoritative)
- `docs/design/` — earlier design tokens (`DESIGN.md`) + Stitch mockups per view
- `docs/adapters/AUTHORING_GUIDE.md` — how to add language-adapter coverage
  (sandboxed WASM plugin, or extending a compiled-in `adapters-lang-*` crate)
- `docs/agent-golden-tasks.md` — the eval set for this repo's agent
  primitives (ENG-0006 item 4); added by #262, growth reason: a new doc, not
  a length increase to an existing one
- GitHub wiki — narrative contributor/process docs (`CONTRIBUTING.md` is a stub)
- `docs/help/` — the single authored copy of in-app Help. The wiki's `Help-*`
  pages are generated from main after merge by `help-wiki-sync.yml`; never
  edit them on the wiki, and never push the wiki to make a PR pass

<!-- governed:shared-agent-discovery:start -->

## Shared agent conventions and skills

PR-first workflow, validation-before-push, commit and PR hygiene, and the
untrusted-input threat model are defined once, for every repo, in the
[org-wide agent conventions](https://github.com/qwts/agent-sop/blob/main/docs/reference/agent-conventions.md).
Before creating or copying a repo-local skill, consult the reviewed
[shared agent skills](https://github.com/qwts/agent-sop/blob/74e775ef23d8e7d8f8e693ccc2329f430978c096/skills/README.md)
index. Reuse only the pinned version supplied by the governed harness; a skill
genuinely specific to this repository belongs in its local context.
This repository is governed by
[agent-sop](https://github.com/qwts/agent-sop) — its
[shared SOPs](https://github.com/qwts/agent-sop/blob/main/docs/sop/README.md)
and [engineering decisions](https://github.com/qwts/agent-sop/blob/main/docs/decisions/README.md)
apply here by default
([ENG-0008](https://github.com/qwts/agent-sop/blob/main/docs/decisions/ENG-0008-shared-sop-inheritance.md):
inherit by default, vary by explicit delta).
<!-- governed:shared-agent-discovery:end -->
