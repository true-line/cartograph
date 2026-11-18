# ADR-0006 — Kuzu graph store + SQLite/WAL spine + content-addressed determinism

- **Status:** Accepted; the graph-store choice is superseded by [ADR-0008](ADR-0008-sqlite-graph-store-kuzu-archived.md) (2026-07-05), which settled the store on SQLite/WAL with recursive-CTE traversal after the M0 check found Kuzu archived upstream. The SQLite/WAL spine and content-addressed determinism decisions stand.
- **Date:** 2026-06-21
- **Deciders:** Chris Kane

## Context
Desktop, no server process. Need fast cross-layer path queries plus durable jobs,
provenance, and reproducible re-ingest.

## Decision
Graph in **Kuzu** (embedded property graph, Cypher-style). Relational/state spine in
**SQLite/WAL** (jobs, provenance log, artifact versions, eval results, config). Every
fact is **content-addressed** (`content_hash`) so re-ingesting a commit is idempotent and
diffs are stable; a CI invariant asserts identical T0 graphs for identical commits.
**Fallback:** SQLite edge table + recursive-CTE traversal if Kuzu fails the M0 fit check.

## Consequences
- No server; fast traversal; reproducible outputs.
- Kuzu maturity is a tracked risk (OQ-3) — fallback de-risks it.

## Alternatives (≤3)
- **SQLite recursive-CTE only** — zero-risk, slower on deep paths.
- **DuckDB + PGQ** — strong analytics, graph-path ergonomics less mature for this use.
