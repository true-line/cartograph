# ADR-0048 — Durable MCP request identity: storage prerequisite

- **Status:** Accepted with amendment, decided by the owner on 2026-11-18 (#1761; see the 2026-11-18 amendment); proposed in #777 / #766
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
execution are subsequent #766 work, outside this decision. Storage namespaces
alone do not establish task authorization.

## Consequences

The migration needs owner review and cannot be opened by older application
versions. Existing App retries and history remain compatible. AC-0323 has
migration coverage.

Since this decision (updated 2026-11-18): `create_mcp_client` creates MCP
namespaced rows, runtime App/MCP collision isolation is implemented and tested
(AC-0322, `mcp_nonce_concurrent_retries_create_one_durable_identity` in
`src-tauri/src/jobs/investigations/mcp_nonce_tests.rs`), and the
`cartograph_task_*` MCP tools ship. Their authority and grant model is
specified in US-0043 (AC-0320..0322) and SPEC-10; its decision record is owed
as a sibling ADR, as the 2026-11-18 amendment of
[ADR-0044](ADR-0044-read-only-mcp.md) states.

## Alternatives

- Keep global nonce uniqueness: unrelated clients can collide.
- Encode trusted identity into caller nonce text: mixes caller-controlled values
  with host identity and risks collision with historical App nonces.

## Amendment — 2026-11-18: accepted; Consequences match main

The owner accepted this ADR on 2026-11-18 (#1761) with this amendment: the
Consequences now describe what has landed since, and point to the decision
record owed for MCP task authority. The storage decision is unchanged.
