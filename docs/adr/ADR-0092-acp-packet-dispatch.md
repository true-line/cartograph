# ADR-0092 — ACP host for reviewed work packets: worktree-confined dispatch, local branch submission

- **Status:** Proposed (first slice implemented: `crates/acp-host`)
- **Date:** 2026-11-23
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #998, #255 (W8b), #766 (H4); ADR-0017 (plugin host),
  ADR-0019 (MCP ingress, ACP execution), ADR-0036 (deterministic first,
  protocols confer no authority), ADR-0066 (agent SDK lanes); SPEC-01
  "MCP ingress, ACP execution", SPEC-11 (execution `unoffered`)

## Context

#998 closes the adapter-gap loop: preflight names an uncovered cell, the
registry finds no adapter, and the generator writes a draft W6 packet with
the adapter contract, target cells, representative files and fixtures to
write first (AC-0361, AC-0362, AC-0371, AC-0369). The last piece is to hand
that packet to a coding agent and get its change back for review. SPEC-01
names ACP as the execution protocol and W8b as its home, but no ACP code
exists in the product.

An ACP agent (Claude Code, Codex, Gemini CLI and others through their ACP
adapters) is a subprocess that speaks JSON-RPC over stdio. The client
creates a session with a working directory, sends a prompt, streams
updates, and answers the agent's permission and file requests. SPEC-01 is
explicit that ACP permissions are not a sandbox and that nothing is
accepted automatically from repository text.

## Decision

1. **A separate `acp-host` crate holds the client side of ACP v1.** It
   spawns the agent command, frames newline-delimited JSON-RPC 2.0 on
   stdio, sends `initialize` (protocol version 1, client file-system read
   and write, no terminal), `session/new` with the worktree as `cwd` and no
   MCP servers, then one `session/prompt` carrying the packet text. It
   answers `session/request_permission`, `fs/read_text_file` and
   `fs/write_text_file`; every other agent request gets JSON-RPC
   `-32601`. Updates are recorded, bounded, as the transcript.

2. **The host confines the agent to one git worktree.** Dispatch adds a
   worktree on a new branch `cartograph/packet-<id prefix>` at the
   packet's base revision, outside the person's checkout.
   - A file read or write resolves the path (symlinks included) and is
     refused unless it lies inside the worktree and outside its `.git`.
   - A permission request is allowed only for a `read`, `edit` or
     `search` tool call whose every location lies inside the worktree;
     every other kind (`execute`, `fetch`, `delete`, `move`,
     `think`, `switch_mode`, `other`) and any call without locations is
     rejected. The host picks an `allow_once` option, never
     `allow_always`; it rejects with `reject_once`, or cancels when no
     reject option is offered.
   - Git runs with `core.hooksPath` set to a directory that never exists,
     so no hook from the repository runs, and the commit is unsigned and
     authored by Cartograph.
   - This is the first slice's policy: the agent writes code and fixtures;
     it does not run commands. Conformance fixtures and CI run on the
     submitted branch, where they already gate every change.

3. **Bounded and cancellable.** The turn has a wall-clock limit, a cap on
   recorded updates and on file bytes per read or write. On the limit the
   host sends `session/cancel`, waits a short grace period for the prompt
   reply, then kills the agent's whole process tree (its process group on
   unix, `taskkill /T` on Windows), which it also does after every turn, so
   nothing the agent started outlives the dispatch or edits the worktree
   while it is committed. Agent output is read through a bounded queue and
   the limit is checked before each line, so a flooding agent neither grows
   host memory nor outlives its limit. The outcome records the stop reason
   (`end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled`)
   or the host's own reason (`timed_out`, `agent_exited`, `protocol_error`).

4. **Submission is a local branch, not a push.** After `end_turn` the host
   commits the worktree's changes on its branch and returns a submission:
   packet id, branch, base and head revision, changed paths, stop reason
   and the transcript. No other stop reason commits. Cartograph does not
   push, open a pull request or merge; the person pushes the branch through
   their own tools and a human approves it before merge, as #998 requires.
   An empty change is a submission with no head commit.

5. **Only a reviewed packet dispatches.** The app offers dispatch for a
   packet whose review status is `reviewed` (SPEC-11), with an agent
   command the person configured. The agent command and its arguments come
   from operator settings, never from packet or repository text. This
   wiring, and the execution state beyond `unoffered`, land in the next
   slice; this ADR fixes the host contract they call.

## Consequences

- #998's last box has a tested host: a scripted ACP agent in the crate's
  tests reads a fixture, writes an adapter file, has an out-of-worktree
  write and an `execute` call refused, and yields a committed branch.
- Real agents need their ACP adapter installed by the person; Cartograph
  installs nothing.
- Agents that insist on running commands get every `execute` request
  rejected. A later slice can allow a fixed conformance command list once
  a runtime boundary (SPEC-01) exists.
- H5 (an MCP caller starting an ACP investigation) reuses the same client;
  only the session's purpose and permission policy differ.

## Alternatives considered

- **Drive each harness's own CLI or SDK.** One adapter per vendor, and no
  shared permission hook. ACP gives one contract and one policy.
- **Push and open the pull request from Cartograph.** Needs repository
  credentials in the app and crosses SPEC-11's "no route invokes GitHub".
  Deferred; a local branch is reviewable with the person's existing tools.
- **Allow commands inside the worktree.** ACP permissions are not a
  sandbox; a command can reach anything the user can. Rejected until a
  real runtime boundary exists.
