# Project Brief: Cartograph — Documentation Reconstruction from Code

> Aligned with the owner's goal restatement of 2026-11-09
> ([VISION](VISION.md), [ADR-0056](adr/ADR-0056-documentation-reconstruction-pipeline.md)).
> Sections 4 and 5 describe what is built; section 2 marks what is planned.

## 1. Overview
Cartograph is a local-first desktop application that reconstructs the documentation an application should have had from day one. You drag in an app, or as much of its code as can be gathered. A deterministic engine traces the infrastructure, code, UI/UX and existing documentation into a provenance-tagged graph. A team of agents then works backwards from those facts to write the ADRs, the product specification and the rest of an enterprise document set. As the code drifts, the documents are brought up to date, so engineers can maintain, rebuild or port the system in a new language or to new requirements.

Cartograph writes nothing to the application it reads and generates no target code. It enables a rebuild; engineers perform it.

## 2. The Pipeline and the Two Modes
| Stage | What happens | State |
| :--- | :--- | :--- |
| **1. Gather** | Register one repository or a whole estate; partial code is accepted and its coverage reported. | Built |
| **2. Trace** | Deterministic recovery across Infrastructure, Cloud, Server, Events and Client, with every fact tagged by tier and evidence. | Built; existing documentation as an input is planned |
| **3. Write** | Agents draft document sections from the facts. Every claim is cited, every draft is reviewed by a human before it enters a document. | Planned (ROADMAP §10) |
| **4. Keep current** | Sections know the facts they rest on and are marked stale and redrafted when those facts change. | Drift detection built; the rest planned |
| **5. Hand over** | The document set, its evidence, the gap register and work packets go to the engineering team. | Work packets and export built; handoff bundle planned |

Cartograph has two modes over the same graph:

*   **Reconstruction:** the full pipeline, for systems whose documentation is missing or stale.
*   **Context hub:** for projects that are already well run. Engineers and agents ask questions, read cited answers and use the graph as a guide to the full picture.

## 3. Core Principles
*   **Fidelity First:** Prefer an explicit **Gap** node over a hallucinated or unverified connection.
*   **Provenance:** Every node and edge in the graph carries metadata identifying its source (file, line, commit) and its producing tier (Deterministic, Dynamic, Semantic, or Agentic).
*   **Inferred is never shown as confirmed:** Agent-written content keeps its tier and its author label after a human accepts it, in the app and in every export.
*   **Human-in-the-Loop:** The engine and the agents propose; the engineer curates. Decisions to accept or reject are persistent.
*   **Local-First & Private:** Analysis happens on-device using Rust and tree-sitter. Cloud LLMs are opt-in per step, with redaction before anything leaves the machine.

## 4. The Escalation Ladder
| Tier | Method | Confidence |
| :--- | :--- | :--- |
| **T0: Deterministic** | Static parse (tree-sitter), IaC HCL/AST graph. | Confirmed |
| **T1: Dynamic** | Observed evidence (Terraform state, OTel traces, test logs). | Confirmed (Observed) |
| **T2: Semantic** | Local embeddings, similarity matching, name/contract clustering. | InferredStrong |
| **T3: Agentic** | Bounded LLM agents proposing resolutions and drafting document sections with cited evidence. | InferredWeak |

## 5. Key Features & Views
### 5.1 Workspace Dashboard
*   Management of repository sets (GitHub App/PAT auth).
*   System topology declaration (`cartograph.system.toml`).
*   Ingest progress monitoring with granular status for each analysis tier.

### 5.2 Atlas (The Unified Graph)
*   Interactive Cytoscape.js canvas visualizing the entire system.
*   Layer filters (Infra, Cloud, Server, Events, Client).
*   **Evidence Panel:** A read-only code viewer providing instant "jump-to-source" for any fact.

### 5.3 Flow Inspector
*   End-to-end business flow tracing starting from user triggers (Screens/Actions).
*   Horizontal sequence diagrams with tier badges for every hop.
*   Explicit **Gap Nodes** where the tracer cannot establish a link.

### 5.4 Spec Workbench
*   Curation surface for generated artifacts (User Stories, ADRs, Data Models).
*   **Gap & Drift Registers:** Tables identifying unresolved hops and conflicts between documentation and code.
*   "Accept / Reject / Annotate" workflow for all inferred proposals.

### 5.5 Investigations and Agent Access
*   Bounded specialist agents answer questions with cited evidence.
*   A read-only MCP surface gives external agents the same context.

## 6. Technical Stack
*   **Shell:** Tauri 2 (Rust core + React/TS/Vite UI).
*   **Graph Store:** SQLite/WAL with recursive-CTE traversal (ADR-0008), plus the state spine.
*   **Parsing:** tree-sitter (Multi-language incremental parsing).
*   **LLM:** Ollama (Local default) with pluggable provider interface.

## 7. Target Audience
Teams that inherit systems whose documentation is missing or stale: Software Architects, Lead Engineers, Product Owners and Security Auditors who need a trustworthy, reproducible and auditable account of a complex, multi-repo system, and the engineers who will maintain, rebuild or port it.
