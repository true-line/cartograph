# ADR-0084 — Journey candidates continue past writes and keep alternatives (`journey-candidate@2`)

- **Status:** Accepted (Cartograph lead as delegate)
- **Date:** 2026-11-22
- **Deciders:** Cartograph lead (delegate); Claude implements
- **Related:** #2261; US-0045 / AC-1310, AC-0352, AC-1309; amends ADR-0050
  decision 4; W11 (#2169)

## Context

ADR-0050 decision 4 stops a path at its first observed-write goal and keeps
one candidate per (entry screen, goal endpoint), the shortest. Both rules lose
real journeys. In the SimpleLogin extension, the user signs in on `/login`
(an observed write on `POST /api/auth/mfa`), sets an API key on
`/api-key-setting`, and only then reaches `/main` and creates an alias. The
graph has every edge, but `journey-candidate@1` stops at `/login`, and the
custom-alias goal keeps only its shortest path, `/` → `/main`, so the API key
screen (W11 step J1) appears in no candidate.

Enumerating every path is not an option: ADR-0050 rejected that, and the
counts grow quickly. The fix has to keep each `@1` candidate and add a
bounded number of paths that say something new.

## Decision

`journey-candidate@2` replaces `journey-candidate@1`. Decision 4 of ADR-0050
holds except for these changes.

1. **A write does not end the path.** A step whose client call reaches an
   observed write is still a goal and still yields its candidate, but the
   search keeps expanding past it. A path that has crossed an observed-write
   step is marked as having done so. Paths that cross no observed write are
   searched first, in the same order and with the same budget use as `@1`;
   write steps and the paths past them are expanded only with the
   expanded-states budget left after that. A method-assumed call on a write
   step does not start a method-assumed candidate, because the step already
   has an observed write.
2. **Primaries are the `@1` paths.** Per (entry, goal) the paths are ranked by
   (crossed an earlier observed write, then path length, step screen ids and
   navigations). A path that crosses a write ranks after every path that does
   not, so the first-ranked path, the primary, is the path `@1` chose wherever
   `@1` found one.
3. **Alternatives through skipped screens.** Walking the remaining paths of a
   goal in rank order, a path is kept as an alternative when it passes through
   a screen that no better-ranked kept path of that goal passes through. That
   keeps the best path through each screen the primary skips, once.
   Every other path is still only counted in `alternatives`.
4. **An alternative is its own candidate.** It has its own `id` (same digest
   inputs as `@1`), records `alternative_of` with the primary's id, and
   `journeys.md` titles it `{label} (via {screens not on the primary})` and
   names its primary. It is InferredWeak like every journey candidate.
5. **Budget.** `JourneyBudget` gains `max_alternatives = 64` per entry screen,
   next to `max_screens = 6`, `max_expanded_states = 256`,
   `max_calls_per_screen = 64` and `max_candidates = 32` (primaries) per entry.
   Alternatives are kept best-ranked first across that entry's goals; when
   more qualify, the rest are dropped, each kept candidate of that entry
   records `truncated: "alternatives"`, and `alternatives_complete` is false.
   The budget, with this field, is hashed into every candidate's id.
6. **Version.** The rule version is `2`, so every journey id changes once.
   Labels and acceptances made against `@1` ids become stale under ADR-0050
   decision 5, never carried over.

## Consequences

- A screen reached only past a sign-in write, such as SimpleLogin's API key
  setup, now appears in a candidate, so W11's J1 can be scored.
- Every `@1` candidate survives as a primary with the same steps and goal.
  Its `alternatives` count can grow, because paths past writes are counted
  too, and its id changes with the version.
- Because `@1`'s paths are searched first, expanding past writes never
  takes budget from them. It can still exhaust the rest, so an entry can
  now record `truncated: "expanded_states"` where `@1` did not.
- Goals reachable only past a write add primaries. When an entry then has
  more than `max_candidates` goals, the best-ranked are kept (every `@1`
  goal ranks first) and the entry records `truncated: "candidates"`.
- The added candidates are bounded per entry by `max_alternatives`.

## Alternatives considered

1. **Keep every simple path to a goal:** unbounded in practice and mostly
   paths that differ only in order; rejected as in ADR-0050.
2. **Only continue past writes, still one path per goal:** the shortest path
   to `/main` skips `/login`, so J1 stays missing.
3. **A second entry at every screen after a write:** recovers the screen but
   loses the path from the real entry, which W11 scores in order.
