# ADR-0052 — Desktop investigation snapshot ceiling for real estates

- **Status:** Accepted — #1602; owner decision 2026-11-01 ("limits as high as needed")
- **Date:** 2026-11-01
- **Deciders:** Cartograph owner; Claude implements, Codex cross-reviews

## Context

Every specialist investigation read the frozen canonical graph under one shared
`SnapshotReadLimits` value: 32 MiB, 50,000 combined node/edge rows, depth 32 and
1,000,000 JSON values. Those numbers were both the default and the hard maximum.
The SimpleLogin pilot and the 61-repository OpenMRS estate (about 330,000 nodes)
exceed them, so a desktop investigation stopped with "Investigation limit
reached" before it could read anything. MCP-started tasks size a per-worker
memory reservation from the same limits (default × live tasks), so raising the
shared default would make every MCP admission reserve many gigabytes.

## Decision

Split the hard ceilings from the default:

- `SnapshotReadLimits::MAX_*` (the operator-validated ceilings) become 4 GiB,
  10,000,000 rows, depth 32 and 500,000,000 values.
- `SnapshotReadLimits::default()` stays at 32 MiB / 50,000 / 32 / 1,000,000.
- `SnapshotReadLimits::investigation()` returns the ceilings and is used only by
  desktop-started investigations (preparation, coverage and inspection reads).
- MCP-started investigation tasks keep the fixed `default()` for acquisition
  and the corresponding worker reservation. `task_graph_limits(true)` does not
  read operator-configured graph limits.
- Separate MCP read sessions may configure graph limits up to the new ceilings;
  their read-session reservation grows with those limits. This does not change
  MCP-started investigation task acquisition.

Desktop investigations already have no host memory admission; the dedicated
analysis machine is expected to supply the memory a large estate needs.

## Consequences

- Real estates can be investigated from the desktop app.
- Local work packets reuse the hard ceilings, so they also admit larger graphs.
- Peak memory for a desktop investigation scales with the graph; a 330k-node
  estate fits well within 4 GiB of serialized graph.
- MCP-started investigation task acquisition and reservation remain unchanged.
  Separate MCP read-session admission and reservation change only when an
  operator changes the read-session graph limits.

### Amendment — 2026-11-10: the post-ingest knowledge phase takes the same ceiling (#1771)

The decision above said `SnapshotReadLimits::investigation()` is used only by
desktop-started investigations. The knowledge phase that runs after ingest read
the base under `default()`, so any repository over 50,000 rows was skipped whole
with a `knowledge-base-bounds` omission and no knowledge node was produced.

The phase now reads under `investigation()` too. The reasoning is the same one
that justified the investigation exception: it is a desktop background read on
the owner's machine, it has no host memory admission and reserves nothing in
advance, and the analysis machine is expected to supply the memory. Its peak
memory therefore scales with the graph, as a desktop investigation's does.

The rule after this amendment: `investigation()` is for desktop-side reads with
no memory reserved in advance (desktop-started investigations and the
post-ingest knowledge phase). Every read that sizes a reservation from its
limits keeps `default()` or its operator-configured value: MCP-started
investigation tasks and MCP read sessions are unchanged. A further caller of
`investigation()` needs another amendment here.

