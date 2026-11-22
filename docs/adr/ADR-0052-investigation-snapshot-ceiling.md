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

### Amendment — 2026-11-10: the desktop context query takes the same ceiling (#1782)

The desktop context query (`query_qualified_context`) read the base under
`default()`, so on a graph over 50,000 rows every query returned an error. It is
the same class of read as the two above: started from the desktop window, on the
owner's machine, with no host memory admission and nothing reserved in advance.
It now reads under `investigation()`.

Consequence: each query page copies the whole base, so on a large graph a query
costs time and memory in proportion to the graph, as it already did below the
old limit. A base past the ceiling is still refused with the bounds error.

The rule is unchanged. `investigation()` now has three callers, all desktop
reads with no reservation: desktop-started investigations, the post-ingest
knowledge phase and the desktop context query. MCP context reads do not use this
path and keep `default()`.

### Amendment — 2026-11-18: the streamed identity pass has its own caps (#1602)

- **Status:** Accepted — decided by the owner 2026-11-18
- **Deciders:** Cartograph owner; Claude implements

Scoped preparation (#1602, AC-0735) computes the complete-graph `context-v1`
identity by streaming the whole graph one row at a time, and keeps in memory
only the facts of the investigation's scope. Under the rule above, an
MCP-started task would stream under `default()`, so any estate over 50,000 rows
would still stop with `host_limit` even though the pass holds one row, not the
graph.

Decision (owner, 2026-11-18): the identity pass and the retained scope take
separate limits (`core_graph::IdentityPassLimits`).

- **Identity pass (aggregate):** the ADR-0052 ceilings,
  `SnapshotReadLimits::investigation()` (10,000,000 rows, 4 GiB raw and
  canonical bytes, depth 32, 500,000,000 values), for desktop-started and
  MCP-started tasks alike.
- **Retained scope:** the task's own limits, unchanged. MCP-started tasks keep
  `default()` (50,000 rows, 32 MiB, 1,000,000 values), so the worker reservation
  sized from it is unchanged; desktop-started tasks keep `investigation()`.
- **The one row the pass holds** is capped by the retained limits too: its raw
  stored bytes are checked against the retained `max_bytes` before its body is
  copied, and its property values and depth against the retained `max_values`
  and `max_depth` while it decodes. A row the retained limits could never have
  admitted is refused (`SingleRowByteLimit`, `SingleRowValueLimit`), so the
  pass never holds more than the reservation already covers.
- `IdentityPassLimits::for_retained(retained)` is the only constructor
  preparation uses. `IdentityPassLimits::uniform` (and a plain
  `SnapshotReadLimits`) keeps exactly the admission of `read_snapshot_bounded`.
- A preparation snapshot refusal names the phase and the limit:
  `Investigation limit reached: host_limit; phase=graph_identity|graph_scope;
  limit=snapshot_rows|snapshot_bytes|snapshot_values|snapshot_depth|snapshot_row_bytes|snapshot_row_values.`
  The message carries no fact, path or count.

Consequences:

- Once preparation is wired to it, an MCP-started task over a real estate gets
  a complete-graph identity instead of `host_limit`. Its memory stays within its
  reservation; its time grows with the whole estate, as a desktop task's does.
- This is an exception for reads that retain one row, not a change to the rule
  above: every read that materializes the graph, or sizes a reservation from
  the graph it holds, still keeps `default()` or its operator-configured value.
  MCP read sessions are unchanged.
- The graph identity itself is unchanged: the pass hashes to the id
  `ContextSnapshot::new` assigns (AC-0735).
- Wired (2026-11-21, AC-1301, AC-1302): `investigations/context.rs` prepares a
  neighborhood or area scope inside a scoped reader at one pinned revision,
  streaming the identity and retaining only the scope, and re-reads selected
  facts at that revision. Wired (2026-11-22, AC-1311): `completion_choices.rs`
  lists and validates a neighborhood or area's choices the same way. Not yet
  wired: `inspection.rs` still materializes the graph to bind a source
  selection; an `All` scope keeps the full bounded read (AC-1301).

