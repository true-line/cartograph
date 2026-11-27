# Cartograph product guide for design

- **Status:** Current as of release 0.81.0 (2026-11-25).
- **For:** designers working on the UI, and anyone who needs to know what
  the app does before changing how it looks.
- **Detail per screen:** the in-app help in [`docs/help/`](../help/) is the
  authoritative description of each surface. This guide is the overview.
- **Visual reference:** [`production-ui/`](production-ui/) holds the original
  handoff (tokens, tier palette, prototype screenshots). It describes nine
  surfaces; the app has since gained Agent Chats, Investigations, Agent work
  packets, Help and quick dispatch, which it does not cover.

## 1. What Cartograph is

Cartograph is a desktop app (macOS, Windows) that turns a messy legacy
codebase into the documentation it should have had: an enterprise document
set and the recovered business rules, good enough to maintain, rebuild or
port the system from, plus topic-expert agents that answer questions about
it with citations.

It works in two stages:

1. **Recover facts.** Deterministic parsers read the code, infrastructure,
   UI and existing documents and build a graph. Every fact records how it was
   found (its tier) and where (its evidence). What cannot be resolved is
   recorded as a **gap**, never guessed.
2. **Write documents from those facts.** Rules compile most sections
   directly from the graph. Agents draft the prose sections, citing facts.
   A person reviews every draft in the Spec Workbench; a draft reaches
   exports, curated reads and MCP only after it is accepted.

Cartograph never writes code into the system it reads.

A well-documented project uses the same graph as a **context hub**: people
and other agents ask questions and get cited answers.

## 2. Who uses it

| User | Goal | Where they spend time |
|---|---|---|
| Modernization engineer or architect | Understand an unfamiliar estate well enough to rebuild or port it | Connect, Atlas, Flows, Gaps |
| Reviewer (tech lead, analyst) | Decide which agent drafts and proposals are true | Spec Workbench, Investigations, Gaps |
| Product owner or business analyst | Read what the system does and its business rules | Spec Workbench, exported documents, topic experts |
| Engineer or coding agent on a live project | Ask questions, get cited answers | Agent Chats, quick dispatch, read-only MCP |
| Admin of the install | Providers, consent, local models, limits | Settings, Jobs |

The hardest and main case is an enterprise estate: many repositories,
outdated or missing documentation, business flows that exist only in code.

## 3. Concepts the UI must show

These are the product. A redesign can change how they look, not whether
they are visible.

- **Tier** says how a fact was produced: **T0** static parse, **T1**
  observed at runtime, **T2** semantic similarity, **T3** agent proposal.
  Higher tiers never overwrite T0/T1.
- **Confidence** is on every fact: Confirmed, Inferred Strong, Inferred
  Weak, Gap. It is never shown by colour alone: always a label or code, and
  a shape where it is a graph node (gap = octagon with dashed border).
- **Gap vs unsupported pattern.** A gap is a fact the evidence points to but
  that could not be resolved; it can be escalated tier by tier. An
  unsupported pattern is a construct no installed adapter reads: a tool
  limitation, fixed by an adapter, not by escalation. These are never mixed.
- **Proposals and drafts are staged.** Agent output (escalation proposals,
  investigation findings, section drafts) waits for a person to accept,
  reject or return it. Accepting changes its review status, never its tier:
  it stays T3 / Inferred Weak and labelled with the agent role that wrote it.
- **Evidence.** Every fact, edge, hop and claim opens its evidence: file and
  span, extractor, tier, confidence, content hash.
- **Export modes.** *Verified-only* leaves out every Inferred Weak statement
  and says how many it left out. *Best-effort* includes them, annotated.
- **Stale.** When a fact cited by an accepted draft changes, the draft is
  marked stale and can be re-drafted. The old text stays until the new one
  is accepted.
- **Egress and consent.** Local-first. Nothing goes to a cloud model unless
  the tier has consent *and* the user approves the exact payload of that one
  step. The status bar always shows bytes sent (`Local-only · 0 bytes
  egress`).
- **Projects.** One loaded project at a time; others are parked and can be
  switched to without re-ingesting.
- **Jobs.** Recovery, escalation, drafting and agent questions run as
  durable jobs that survive restart and can be cancelled, retried or resumed.

## 4. The app shell

Present on every surface:

- **Left rail** with the primary surfaces. The first eight are `Cmd+1`–`Cmd+8`
  on macOS, `Ctrl+1`–`Ctrl+8` on Windows.
- **Header** with breadcrumb, the loaded system's name and the Help entry.
- **Status bar** with the egress figure and background activity.
- **Command palette** (`Cmd+K` / `Ctrl+K`): navigation only, to the rail
  surfaces, Investigations and Help. It runs no other commands and does not
  list the ingest steps or Agent work packets.
- **Evidence drawer**: opens from any fact, read-only.
- **Legend popover**: the tier and shape key.
- **Knowledge phase notice**: a banner after an ingest when the derivation
  step skipped work, naming each reason. Dismissible.
- **Quick dispatch** (`Cmd+Space` or `Cmd+J` on macOS, `Ctrl+J` on Windows):
  a dialog to send a question to an agent from anywhere. A "Dispatch to an
  agent" pill advertises it until first use.
- **Egress consent dialog**: modal, shows the exact payload before any cloud
  call.

## 5. Surfaces

Each row links the help page that describes the surface in full.

| Surface | What it is for | Main contents | Main actions |
|---|---|---|---|
| [Connect → Preflight → Recover](../help/ingest.md) | Bring a system in | Target picker (GitHub repo, local folder, system manifest, drag-and-drop); preflight scan of languages, frameworks, adapter coverage and risky constructs; recovery job with per-language counts | Connect, run preflight, start/cancel recovery, create a manifest |
| [Workspace](../help/workspace.md) | Landing page: what was recovered and how well | Outcome (Full / Partial / Inferred), system contents per repo with commit and graph hash, coverage inventory, provenance health, artifact list, projects card, "Investigate this system" | Triage gaps, open provenance, new project, switch/rename/delete project, clear system |
| [Atlas](../help/atlas.md) | See the whole system | Graph in five bands (Infrastructure, Cloud, Server code, Events, Client code), clusters that expand, confidence overlay | Find entity, focus on a node (Enter / Esc), open evidence |
| [Flows](../help/flows.md) | Follow a user action through the system | Each flow from screen or endpoint, hop by hop, with tier and evidence per hop; gaps truncate the branch | Toggle verified-only / best-effort, open a hop's evidence or a gap's resolution strategy |
| [Spec Workbench](../help/spec.md) | Read and curate the documents | Artifact list (product specification, user stories, flow dossiers, ADRs, endpoint inventory and permissions, pipelines, configuration keys, external services, environments, documentation evidence, C4 diagrams, curated items, coverage, fact index, bundle manifest); sections and drafts; curation log; **Ask an area's expert** | Draft / re-draft a section, accept / reject / return a draft, annotate, export the bundle, copy diagram source |
| [Gaps & Drift](../help/gaps.md) | Work through what could not be confirmed | Three lanes: system gaps, unsupported patterns, no-evidence questions; drift tab; gaps grouped by cause class at scale | Escalate one gap or a class locally, open the resolution strategy, accept / reject proposals |
| [Provenance & Eval](../help/prov.md) | Measure the honesty | Tier distribution, per-extractor coverage, eval gates, re-ingest history with content hashes | Read only |
| [Jobs](../help/jobs.md) | Background work | Every job with stages and progress | Cancel, retry, resume, clear finished |
| [Agent Chats](../help/agent-chats.md) | Saved agent conversations | Agent pool (queued/running agent work, providers, token usage, controls), chat sessions, docked chat panes with cited findings | Open a chat, ask a follow-up, pause or limit agent work, search saved answers |
| [Investigations](../help/investigations.md) | Full composer for specialist investigations | Specialist roles, question composer with scope and evidence, saved tasks, findings, conversation turns | Start, retry, open findings, use for follow-up, accept as curated |
| Agent work packets | Hand reviewed context to an external coding agent | Saved packets, packet detail by group, review by revision | Create, review, dispatch to the configured ACP agent |
| [Settings](../help/settings.md) | Tiers, providers, consent, adapters | Tier toggles (T0 always on), provider keys (stored in macOS Keychain / Windows Credential Manager), local lane (Ollama) status, read-only MCP, run limits, installed adapters | Save key, check connection, grant/revoke consent, check local lane |
| Help | In-app copy of `docs/help/` | One topic per surface, plus first run and concepts | Read |

Modals reused across surfaces: the **Resolution Strategy** modal (escalate a
gap tier by tier, see what each tier would send), the **egress consent**
dialog, and the **evidence drawer**.

## 6. User journeys

### J1. First recovery (no model needed)

1. Install and open the app (see [first run](../help/first-run.md)).
2. Connect: pick a GitHub repo or a folder, or drop folders on the window.
3. Preflight shows what can and cannot be read. For a GitHub repo it shows
   "Detection deferred", which is expected.
4. Recover runs as a job. The summary shows files, nodes and edges per
   language, including zeros.
5. Land on Workspace: outcome, coverage, provenance health.

Design note: this is the first impression and today runs as three routed
views that light the Workspace rail item rather than a visible step flow.

### J2. Understand the system

Workspace → Atlas (whole system, focus into a cluster) → Flows (one user
action end to end) → evidence drawer on any node or hop → Spec Workbench
C4 diagrams for the architecture view.

### J3. Work the gaps

Workspace "Triage gaps" → Gaps & Drift → pick a cause class → open the
Resolution Strategy → escalate locally (or to cloud with consent) → a
staged proposal per instance → accept or reject each. Nothing joins the
graph unaccepted.

### J4. Produce the product specification

Spec Workbench → product specification → **Sections and drafts** → "Draft
this section" (opens the composer with the Section Writer role) → run →
read the draft's claims and citations → accept, reject or return → the
accepted section appears in the best-effort export. Later, after a
re-ingest, a section whose cited facts changed shows **stale** with the
changed facts listed → "Re-draft this section" → compare with the replaced
draft → accept.

Section kinds the UI distinguishes: **P** projected (compiled, no model),
**A** authored (agent draft, human accepted), **P+A** framed (compiled frame
with authored prose), **H** human (a labelled blank Cartograph never fills).
See the [document catalogue](../DOCUMENT-CATALOGUE.md).

### J5. Ask a question

Three entry points to the same investigation machinery: quick dispatch from
anywhere, "Ask an area's expert" in the Spec Workbench (one expert per
recovered area), and the full Investigations composer. Answers land in
Agent Chats with findings and citations. A finding can be accepted as
curated, which lets exports and MCP reads show it, still as T3.

### J6. Hand off

Spec Workbench → export (verified-only or best-effort) → a bundle whose
`bundle-manifest.md` lists every file and the exact graph and revisions it
was made from. Other agents read the same content through the read-only
MCP surface, or receive a reviewed agent work packet.

### J7. Turn on a model provider

Settings → local lane (Ollama, nothing leaves the machine) or provider keys
(cloud) → check connection → set a tier to cloud → disclosure and tier
consent → each cloud step later asks for per-step consent with the exact
payload. The status bar egress figure updates.

### J8. Several systems

Workspace → New project (parks the loaded one) → recover another system →
Projects → Switch back with no re-ingest. Settings, keys and local models are
shared across projects.

## 7. What the user gets out

The export bundle, built from the graph and accepted curation only:

- `product-specification.md`: the D1 document in catalogue order, each
  section marked P, A, P+A or H, with stale sections listed in the header.
- Flow dossiers, user stories, ADRs, topology, data model, endpoint
  inventory and permissions, pipelines, configuration keys, external
  services, environments, documentation evidence, rule evidence, test
  arrangements, coverage.
- Gap register and drift register.
- `fact-index.md`: every cited fact with its evidence.
- `bundle-manifest.md`: what is in the bundle and what it was built from.

Documents D2 to D10 of the catalogue (architecture overview, API reference,
security, operations, test strategy, user journeys, onboarding) are not yet
agent-drafted ([#1748](https://github.com/true-line/cartograph/issues/1748)).

## 8. Rules a redesign must keep

1. Never show confidence by colour alone. Pair colour with a label or code,
   and with shape on graph nodes.
2. Never present a T3 / agent statement as confirmed, including after a
   person accepts it. Keep the author role and review state visible.
3. Gaps are first-class: visible in graphs, flows and documents, never
   hidden or filled in.
4. Keep gaps and unsupported patterns visually and structurally separate.
5. No cloud call without the consent dialog showing the exact payload; the
   egress figure stays on screen.
6. Every fact, hop and claim reaches its evidence in one step.
7. Deterministic (T0) content cannot be edited away in the UI, only
   annotated.
8. Shortcuts and labels follow the platform: `Cmd` on macOS, `Ctrl` on
   Windows; quick dispatch is `Ctrl+J` on Windows.
9. Long work is a job: show progress, allow cancel, and survive a restart.
10. Large estates: views must stay usable with thousands of nodes (Atlas
    caps a band at 60 tiles and groups the rest).

## 9. Where the current UI is known to strain

Observed from the code and help, for the designer to weigh:

- The Spec Workbench carries more than fifteen artifacts, section drafting,
  curation, the topic experts and diagrams in one surface.
- Investigations and Agent Chats read the same saved history from two
  surfaces, and quick dispatch is a third entry point.
- The ingest flow (Connect, Preflight, Recover) is not on the rail; it lights
  Workspace.
- Agent work packets are reachable but not a rail surface; the rail lights
  Spec for them.
- Many panels state limits and caveats in long inline text (see the help
  pages), which the UI repeats.
