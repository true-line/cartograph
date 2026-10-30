# ADR-0051 — Layered knowledge model: deterministic base, knowledge layer, artifact-only targets

- **Status:** Accepted (owner, 2026-10-30)
- **Date:** 2026-10-25
- **Deciders:** Cartograph owner; implementation agents (Claude, Codex)

## Context

The owner restated the product's structure on 2026-10-25 as three layers:

1. **Deterministic base.** Facts recovered from source by T0 adapters and T1
   observations. It is the authority and is correct as built.
2. **Knowledge layer.** A knowledge graph and retrieval (RAG) over embeddings
   that *augments* the base. It was always meant to exist (VISION V3) and
   never to replace the base.
3. **Artifact-only targets.** Applications whose source is lost or
   incomplete, where only binaries or deployed artifacts exist (VISION
   "Beyond v1", ROADMAP §8 item 7).

Today's facts:

- **Graph store.** `core-graph` exposes a `GraphStore` trait whose only
  implementation is SQLite in WAL mode with recursive-CTE traversal
  (ADR-0008; Kuzu archived). ADR-0047 (Proposed) adds the revision token.
  Every node and edge carries `core-prov` provenance: tier, confidence,
  evidence spans and extractor.
- **Embeddings.** `crates/semantic` embeds through a local Ollama
  `/api/embed` endpoint (`nomic-embed-text`) and builds an in-memory `usearch`
  ANN index per invocation. It serves only the semantic preview (candidates
  and unresolved hops). ADR-0037 caches vectors in a bounded
  SQLite store. No ingest phase embeds a codebase, and nothing indexes text
  lexically.
- **Knowledge nodes.** Nothing outside `crates/spec` produces `Capability`,
  `BusinessFlow`, `DomainEntity` or `Actor` nodes (ROADMAP §2), so the
  user-stories artifact is empty on real ingests.
- **Artifacts.** No adapter reads a binary, bundle or image. #995 measured a
  `.d.ts` reader for dependency declarations and found it closes none of the
  targeted gap classes on `ui/`.

The question is which stores and contracts serve all three layers without a
service dependency, while keeping the base authoritative.

## Decision

1. **The deterministic base stays the system of record.** SQLite through
   `GraphStore` remains the only authoritative store. No layer above it
   updates, deletes or re-tiers a base fact. A server graph database (for
   example Neo4j) is not adopted: it needs a JVM service, breaks drop-in
   desktop use and adds nothing to the provenance and determinism contracts
   already built on SQLite. A later shared estate server may *export* to one;
   it is not a store of record.

2. **The knowledge layer is a separate, derived and disposable overlay.**
   Its facts never enter the deterministic graph or its content-hash
   identity, so re-ingesting a commit yields the same base graph whether or
   not a provider was available (AGENTS.md determinism invariant). The
   overlay is stored and hashed on its own, and its identity binds the base
   graph revision (ADR-0047), the versioned recipe, the ADR-0037 embedding
   namespace and whether the provider phase ran. Its facts are:
   - embeddings and retrieval at T2 (`InferredStrong` ceiling);
   - knowledge nodes (`Capability`, `DomainEntity`, `Actor`,
     `BusinessFlow`) produced first by deterministic rules where they apply,
     then by clustering over embeddings (T2);
   - agent labels (T3), which are **staged proposals** under R-INT-3, never
     written as overlay facts directly. A label reaches the overlay only
     through an explicit acceptance step into a curated projection, keeps its
     Agentic/`InferredWeak` provenance there, and is excluded from
     `verified-only` exports (R-INT-5).

   Every overlay fact cites the base facts it rests on. Reproducibility is
   semantic, not byte-identical by default: rebuilding from the same base,
   recipe and *pinned cached vectors* with deterministic index and
   clustering settings yields the same overlay. Without pinned vectors,
   ADR-0037 does not attest deterministic provider execution, so a rebuild
   may differ and is reported as a new overlay identity. Accepted labels are
   reproducible only from their recorded receipts.

   This extends ADR-0037, which keeps T2 an ephemeral overlay that rebuilds
   ANN membership from the current graph: ANN membership is still rebuilt
   from current inputs and never authoritative; what is new is a persisted,
   separately hashed overlay of derived facts. Graph clear, schema reset and
   retained-source forget invalidate overlay facts and index membership as
   well as the cached vectors ADR-0037 already covers.

3. **Embeddings become an ingest phase, after the base.** Units are symbols,
   endpoints, flows, docs, ADRs, and issue and commit text, chunked by a
   versioned recipe after deterministic redaction. Vectors persist under
   ADR-0037's identity and bounds in SQLite. The `usearch` index is built
   from those rows and the current overlay inputs, and is a rebuildable
   cache, never authority. Providers
   are local by default (ADR-0037 identity rules apply). A run with no
   provider skips the phase and reports it as a named omission.

4. **Retrieval is hybrid and graph-first.** A query first resolves exact
   anchors and their graph neighbourhood deterministically, then ranks text
   with SQLite FTS5 (lexical) fused with vector similarity. Every answer
   cites base facts. Retrieval results carry the T2 tier and are never
   written back into the base graph.

5. **Artifact-derived facts are a third evidence basis.** Provenance gains an
   evidence basis distinct from tier: `source`, `artifact` or
   `observation`. An `EvidenceRef` for an artifact cites the artifact's
   content digest, its kind and an offset or member path instead of a source
   span. Readers are adapters like any other, behind the coverage matrix
   (W9) and the plugin host, so they extend the same way languages do:
   - bytecode and IL (JVM class files, .NET assemblies, `.pyc`): types,
     methods and call sites read directly;
   - JavaScript bundles: source maps when present, then routes and network
     calls from the bundle;
   - native binaries: symbol tables, imports and strings; disassembly only
     through a separately declared tool;
   - container images and deployments: SBOM, layers, entrypoints, manifests
     (Terraform state and OTel traces already enter through T1).

   A static read of an artifact may assert `Confirmed` for structural facts
   (this class has this method) under the `artifact` basis. Business rules,
   journeys and infrastructure roles are never confirmed from artifacts
   alone (VISION "Beyond v1"); they stay observed evidence that can support
   or contradict a source claim. Where source and artifact disagree for the
   same revision, the disagreement is a drift finding, not an overwrite.

## Consequences

- No new service: the desktop app keeps one SQLite file plus a rebuildable
  vector index, and the base graph's content-hash identity is unchanged by
  the overlay.
- The empty knowledge-node artifacts get producers with an explicit tier
  ladder, and each producer is testable on its own.
- `core-prov` needs an evidence-basis field and an artifact `EvidenceRef`
  shape. That is a schema change with a migration, scheduled with the first
  artifact reader, not before.
- Recursive CTEs may not serve whole-estate graph analytics (centrality,
  community detection). When a measured need appears, an in-memory graph
  engine loads a derived copy at query time. The `GraphStore` trait keeps
  the Kuzu-fork escape hatch from ADR-0008.
- Embedding cost and provider identity follow ADR-0037. Its acceptance
  (2026-10-30) lifts the earlier condition that the ingest phase be built
  behind it; the phase still runs only with a configured embedding provider.

## Alternatives

- **Neo4j as the graph store:** the strongest server graph database, but a
  JVM service, and the provenance, determinism and revision contracts would
  have to be rebuilt on it.
- **Another vector store:** Chroma is a Python service that adds nothing over
  `usearch` plus SQLite rows for a local app. LanceDB (embedded, strong
  metadata filtering) and sqlite-vec (one transactional file, no approximate
  index) stay candidates if filtered search at estate scale or `usearch`
  persistence is measured to fall short.
- **Embeddings as the primary index:** rejected by VISION V2/V3:
  deterministic facts first, embeddings as retrieval and inference aids.

## Delivery

Each layer gets its own epic and acceptance, filed after this ADR is
accepted:

- **Knowledge layer:** the embeddings ingest phase, hybrid retrieval, then
  knowledge-node producers. The first acceptance is a SimpleLogin capability
  set scored against an expected set written before the run.
- **Artifact-only targets:** the evidence-basis schema with a first reader
  (JVM bytecode, which needs no external tool), then bundles with source
  maps. The first acceptance recovers a known jar's structure and diffs it
  against its own source as the oracle.
