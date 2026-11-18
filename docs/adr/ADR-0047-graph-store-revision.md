# ADR-0047 — Graph store revision

- **Status:** Accepted, decided by the owner on 2026-11-18 (#1761); proposed in #744
- **Date:** 2026-10-22
- **Deciders:** Cartograph owner; Codex implements, Claude cross-reviews shared semantics

## Context

Bounded readers need to reject continuation when facts, source associations or
coverage change without acquiring the entire graph. Existing content identities
remain meaningful but cannot cheaply qualify a primary-key window. A revision
must also observe supported writes from an already-open older connection.

## Decision

1. Use an additive exact-validated private schema with guarded row triggers on
   nodes, edges, source-node bindings, source-edge bindings and coverage
   inventories. Every supported committed row event advances an opaque token in
   the same SQLite transaction; rollback restores data and token together, and
   signed-counter exhaustion refuses the whole statement with a typed error.
2. Pair the token with the durable database instance identity. Revision is
   database-local mutation order, not a content hash, operation count, confidence
   claim or completed-recovery status. Supersession requires an explicit
   completed source/operation/capture binding on top of revision; this counter
   alone cannot establish it. No cross-store atomicity is implied.
3. Validate the complete exact private schema, including bounded metadata before
   copying SQL. Preserve compatible legacy facts, bindings, coverage and instance
   identity during additive initialization. Reject partial, future and foreign
   schema rather than silently repairing it. Supported data writes through an
   already-open older connection fire the installed triggers; hostile schema
   rewrites are outside this contract.
4. Observe initialization version/reset decisions under one immediate
   transaction. Destructive reset, including an empty reset, advances revision
   atomically; failure preserves prior publication state and version. Read the
   token and requested bounded data in one coherent read transaction. Validate
   instance type/length before allocating its String.
5. Adding a future sixth watched table requires an explicit exact-old-to-new
   transactional private-schema migration. Do not silently append it to
   `WATCHED`: existing exact schemas would otherwise become incompatible without
   an admitted migration, and publication could escape the intended revision
   contract. The migration must validate the exact prior schema, replace the
   complete trigger set atomically, preserve the instance and monotonic token,
   and define rollback and counter-exhaustion behavior.

The detailed contract, supported writer model, bounds and measurement caveats
live in [Atomic graph generation](../design/graph-generation.md). Acceptance is
AC-0302/T-0302 and AC-0303/T-0303 under US-0020.

## Consequences

Bounded acquisition can require a storage revision without materializing every
fact. One transaction may advance the token many times; callers cannot interpret
its difference as a recovery or operation count. Trigger work has an observed
cost documented separately from end-to-end MCP performance. Scoped acquisition
and partition artifact compilation remain separate deliverables; this ADR does
not claim either is implemented by #744 or promote their acceptance gates.

## Alternatives

- Increment in each application writer: misses writes from already-open older
  connections and makes correctness depend on every caller remembering the step.
- Hash the complete graph on every read: defeats bounded acquisition and omits
  association-only changes unless the identity contract is changed.
- Hold one read transaction across client requests: retains database resources
  for an unbounded client lifetime and prevents independent request admission.
