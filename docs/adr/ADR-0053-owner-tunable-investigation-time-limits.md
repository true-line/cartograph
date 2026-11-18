# ADR-0053 — Owner-tunable investigation time limits

- **Status:** Accepted, decided by the owner on 2026-11-18 (#1761); proposed in #1633 with owner decisions of 2026-11-02 ("Menubar Configure… window", "Time limits only, before v1", "60 min task / 10 min per call")
- **Date:** 2026-11-02
- **Deciders:** Cartograph owner; Claude implements the host and window, Codex implements the policy validation and cross-reviews
- **Relations:** amends [ADR-0040](ADR-0040-investigation-policy-profiles.md)'s time-cap clause

## Context

The investigation policy registry is closed: a task's limits must equal one
registry entry field for field (`InvestigationPolicy::for_limits`), and callers
cannot construct a policy from their own numbers. In the v0.60.0 SimpleLogin
pilot, step 8 stopped with `deadline_exceeded` when a local model used the whole
600-second active budget. Changing that budget meant a rebuild. The owner asked
for a way to raise time limits from the app, in a window of its own rather than
the app Settings page.

## Decision

1. Exactly two values become owner-tunable, and only upward from the shipped
   default:
   - the task active-time budget (`active_seconds`): default 600 s, maximum
     3,600 s;
   - the timeout for one model call (`request_timeout_ms`): default 180,000 ms,
     maximum 600,000 ms.
   Every other limit (model calls, tool actions, evidence, tokens, bytes,
   consent wait, wall time) stays fixed in the registry.
2. The values are stored in the app settings store (`run_limit_settings`), with
   `NULL` meaning "shipped default", and edited only from **Configure → Run
   Limits…** in the menubar. Values outside [default, maximum] are refused and
   nothing is written.
3. A task records its effective time limits when it starts. Validation still
   requires every non-time field to match the registry entry exactly; the two
   time fields must lie in [registry default, maximum above]. Reopen, replay and
   saved results use the recorded values, so changing the window never alters a
   started or finished task.
4. The bounded-completion ceiling for `request_timeout_ms` rises to the same
   600,000 ms maximum. The default stays 180,000 ms.

## Consequences

- Slow local models can finish without a rebuild; defaults are unchanged for
  everyone who does not open the window.
- The wall-time cap (3,600 s) still bounds a task, so a 60-minute active budget
  can be cut short by consent waits; that is intended.
- Acceptance and pilot records must state the effective time limits used,
  alongside the model and digest.
- Raising any other limit, or allowing values below the default, needs a new
  decision.
