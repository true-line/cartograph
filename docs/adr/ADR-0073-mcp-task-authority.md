# ADR-0073 — Authority and grants for MCP-started investigation tasks

- **Status:** Proposed; owner review
- **Date:** 2026-11-18
- **Deciders:** Cartograph owner; Claude drafts from the implementation on main
- **Related:** #1761; #766; US-0043 / AC-0320..0329; [SPEC-10](../SPEC-10_specialist-investigations.md);
  [ADR-0028](ADR-0028-specialist-investigation-coordinator.md),
  [ADR-0044](ADR-0044-read-only-mcp.md) (read surface),
  [ADR-0048](ADR-0048-mcp-investigation-authority.md) (storage),
  [ADR-0052](ADR-0052-investigation-snapshot-ceiling.md),
  [ADR-0053](ADR-0053-owner-tunable-investigation-time-limits.md); R-INT-3

## Context

ADR-0044 decides the app-owned MCP read surface. ADR-0048 decides only the
storage prerequisite for MCP request identity (the client registry and the
`(request_namespace, nonce)` key) and grants no execution authority. Main now
also exposes five durable task tools to an MCP client:
`cartograph_task_start`, `_status`, `_events`, `_result` and `_cancel`
(`TaskTool` in `crates/mcp-wire/src/tools.rs`, dispatched by
`src-tauri/src/investigations/mcp_dispatch.rs`). Starting a task can run a
provider, so these tools are authority the read-only decision does not cover.
The owner's 2026-11-18 review of ADR-0044 and ADR-0048 (#1761) asked for a
separate record of who may use them and how that permission is given.

The behaviour is specified in US-0043 and partly accepted (T-0320..T-0329 are
all PARTIAL in `docs/test-map.md`). This ADR records the authority model as it
is implemented on main. It adds no behaviour. Questions the implementation does
not settle are listed separately for the owner, under Open questions.

## Decision

1. **Identity is durable; authority is not.** An MCP principal is an opaque
   16-byte id that the host generates (`randomblob(16)`) when the owner runs
   `mcp_task_client_create` in Settings. The registry
   (`investigation_mcp_clients`) holds only the id and its creation time: no
   credential, grant or consent. The owner sets the registry's capacity when
   creating a principal; a full registry refuses creation but still lists the
   principals it holds. A client can never name, create or choose a principal.

2. **A grant is a runtime activation, given only by an owner app action.** The
   owner command `mcp_tasks_enable(policy, client_id, maximum_identities)`
   checks that the principal is registered, and only then builds
   `McpTaskActivation::granted` and opens the endpoint
   (`Service::enable_tasks`, `src-tauri/src/mcp_reads/service.rs`). The grant
   lives only in memory. The default activation (`mcp_reads_enable`) is
   read-only. Restart, disable, shutdown and dropping the activation all revoke
   the grant. A persisted principal survives restart, but without execution
   authority, so the owner must grant again.

3. **One grant per activation, shared by its sessions.** Each authenticated
   connection gets a `McpTaskSession` that shares the activation's revocation
   state, and a session is passed to the worker only after the transport
   handshake. The task tools appear in `tools/list` only while that session's
   grant is live. Every connection on a task-granted endpoint acts as the one
   selected principal. Authentication is ADR-0044's same-account credential
   boundary.

4. **The grant is checked at every durable operation, not just at connection.**
   Start, early retry lookup, status, events, result and cancel each run inside
   `McpTaskSession::with_grant`. The grant stays held for that bounded
   synchronous storage operation, and a revoked or read-only session refuses
   before any transaction opens. Revocation waits for an operation already
   inside `with_grant` and refuses every later one. The principal id and the
   identity cap are read from the grant, never from the wire.

5. **Rotation drops authority until the owner reconfirms.** Owner-requested
   credential rotation (`mcp_reads_rotate`) revokes the old sessions and builds
   a read-only successor (`read_only_successor`). The successor keeps the
   selected principal only so the owner can grant it again. A cancelled
   rotation, or a disable or shutdown during rotation, cannot restore access.
   Settings shows the observed grant and clears it when the lifecycle changes or
   a refresh fails.

6. **A client sees and controls only its own tasks.** MCP tasks are stored in
   the principal's own request namespace, and App tasks keep `app`.
   Status, events, result and cancel first check, in the same transaction, that
   the task belongs to the principal's namespace
   (`require_request_owner`). A foreign task, an App-created task and a missing
   task all give the same refusal. Cancellation takes its task family from the
   stored task, not from the caller. The owner's App inspection and
   cancellation are not limited by principal.

7. **Retries are idempotent per principal.** The same principal, nonce and
   normalized intent always return the one task, even under concurrent requests
   and after graph, provider or capacity changes. A different intent under the
   same nonce is refused. Other principals and App rows cannot collide with
   that namespace. A retry is recovered before any mutable context, provider or
   capacity check, and does not need a new execution slot.

8. **Start goes through the existing coordinator, under fixed host bounds.** An
   MCP start accepts only the closed `{request}` envelope, decoded as a
   `StagedObligationRequest`. That request is untrusted until the host validates
   it. The same coordinator as an App start runs it (`completion_start::start_access`), and
   the specialist must be in the advertised catalog. New work also needs all of
   the following:
   - a detached task slot on the connection. A connection holds at most
     `MAX_LIVE_TASKS` (2), and each slot is kept until the worker exits.
   - room under the principal's identity cap. That is the `maximum_identities`
     the owner set at grant time, compared with the number of stored task rows
     in the principal's namespace.
   - the fixed MCP-task limits: the default graph acquisition from ADR-0052 and
     the shipped default local model and run limits. The desktop Run Limits
     setting (ADR-0053) and the desktop local-model setting do not apply.

   Disconnect, disable or revocation stops new admission and does not cancel
   admitted work. Explicit cancel and restart recovery use the existing
   coordinator, which never repeats a provider call whose outcome is unknown.

9. **No consent and no graph authority over MCP.** No task tool approves a
   consent step, and the closed envelopes reject extra consent or authority
   fields. Cloud egress stays under the existing per-step App consent, bound to
   the exact task, step, revision and payload. Findings stay
   T3/InferredWeak and propose-only (R-INT-3). The relay has no database or
   provider access, and no task operation writes to the graph.

## Consequences

- External agents can run durable investigations only after an explicit owner
  grant in the running app, and lose that ability on restart, rotation, disable
  or revoke. Work they already started keeps running and stays visible to them
  once they hold a grant for the same principal again.
- Keeping principal identity separate from grant means a principal's task
  history and nonce space survive reconnects and rotation, while none of that
  stored state can authorize anything.
- The model rests on the same-account trust boundary from ADR-0044. Any process
  that can read the endpoint credential acts as the granted principal.
- Acceptance is incomplete. T-0320..T-0329 are PARTIAL, AC-0326's host consent
  behaviour is not yet shown, and AC-0329's relay fixture uses a synthetic
  provider. Task tools are Unix-only; `mcp_tasks_enable` returns
  `UnsupportedPlatform` elsewhere, as ADR-0044 decision 5 requires.

## Alternatives

1. **Persist the grant with the principal:** rejected by the implementation.
   A stored grant would turn durable identity into standing authority and would
   survive restart without the owner present.
2. **Let the client name its principal or carry authority in the request:**
   rejected. Caller-chosen identity can collide or be spoofed, which
   ADR-0048 already rejects for nonces.
3. **Check the grant only at connection time:** rejected. A revoke would not
   take effect until the connection closed. Checking at every operation makes
   revocation apply to the very next storage access.

## Open questions for the owner

These are not decided by this ADR. Each records what main does today.

1. **Cloud from MCP (decided by the owner, 2026-11-18): keep it.** The wire
   request chooses `provider_mode`, so an MCP client can ask for a cloud task.
   Egress still waits for per-step consent in the App. AC-0326's host consent
   behaviour must still be proven (T-0326 PARTIAL) before this ADR is accepted.
2. **Grant breadth.** The grant is all-or-nothing over the five tools and every
   advertised specialist and scope. Should it be narrower, for example to
   specialists, scope or provider mode?
3. **One principal per endpoint.** Every connection on a task-granted endpoint
   acts as the same principal. Are several clients sharing one principal under
   one grant intended, or should each client get its own principal at once?
4. **Identity cap semantics (owner direction, 2026-11-18).** `maximum_identities`
   is meant as a local setting, sized to the machine's hardware, that limits
   live (non-terminal) tasks. It is not a lifetime quota. **Code mismatch:**
   main counts every stored task row for the principal, terminal ones included
   (`SELECT count(*) FROM investigation_tasks WHERE request_namespace=?1` in
   `src-tauri/src/jobs/investigations.rs`), so today it behaves as a lifetime
   quota for as long as rows are kept. A follow-up slice counts only live tasks
   and gives the setting a hardware-derived default and bound. Still open: the
   exact default and bound.
5. **Fixed MCP limits.** MCP tasks ignore the desktop Run Limits (ADR-0053) and
   local-model settings and use the shipped defaults. Should either setting, or a
   separate MCP setting, apply to them?
6. **Acceptance gate and story status.** US-0043 is still `Planned`, and its
   Trace still lists MCP task creation as planned, although the tools ship.
   Which of T-0320..T-0329 (in particular a real-provider or installed-client
   run) must pass before this ADR is accepted, and should US-0043's status and
   Trace be updated now?
7. **Placement.** The 2026-11-18 amendments to ADR-0044 and ADR-0048 proposed
   in open PR #1921 (#1761, not yet on main) say this record is owed as a
   sibling ADR or as an ADR-0048 follow-up. This draft
   is a sibling. Should it instead become an amendment to ADR-0048?
