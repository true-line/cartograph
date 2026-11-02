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
- MCP-started tasks keep `default()` (or their operator/scope limits) for both
  acquisition and reservation, so the MCP worker reservation is unchanged.
  Operators may now configure MCP graph limits up to the new ceilings; their
  reservation grows with what they choose.

Desktop investigations already have no host memory admission; the dedicated
analysis machine is expected to supply the memory a large estate needs.

## Consequences

- Real estates can be investigated from the desktop app.
- Local work packets reuse the hard ceilings, so they also admit larger graphs.
- Peak memory for a desktop investigation scales with the graph; a 330k-node
  estate fits well within 4 GiB of serialized graph.
- MCP admission and reservation numbers do not change unless an operator raises
  their configured limits.
