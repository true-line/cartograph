# ADR-0048 — Durable MCP request identity: storage prerequisite

- **Status:** Proposed — #777 / #766; owner review required
- **Date:** 2026-10-23
- **Deciders:** Cartograph owner; Codex implements, Claude cross-reviews

## Context

App investigations currently have globally unique caller nonces. Durable MCP
work will need a separate host-assigned client identity so reconnects can recover
the same task without colliding with App requests. This decision covers only the
storage migration; it grants no execution authority and enables no MCP task API.

## Decision

Migrate the exact supported investigation schema from version 2 to 3 atomically.
Add an initially empty client registry containing only opaque IDs and creation
times. Replace global nonce uniqueness with `(request_namespace, nonce)` uniqueness.
Existing and newly created App rows use the default namespace `app`.

Preserve task record bytes, hashes, IDs, nonces, job/conversation identities,
ordinals, AUTOINCREMENT high-water, and optional read/admission extensions.
Validate foreign keys before commit; restore connection pragmas on success or
failure. Reject unknown layouts. Older writers fail their exact schema checks.

Principal creation, ownership-aware lookup, grant/rotation controls and MCP task
execution are subsequent #766 work. Until then, no supported API creates MCP
namespaced rows. Storage namespaces alone do not establish task authorization.

## Consequences

The migration needs owner review and cannot be opened by older application
versions. Existing App retries and history remain compatible. AC-0323 has
migration coverage; runtime App/MCP collision isolation remains unimplemented.

## Alternatives

- Keep global nonce uniqueness: unrelated clients can collide.
- Encode trusted identity into caller nonce text: mixes caller-controlled values
  with host identity and risks collision with historical App nonces.
