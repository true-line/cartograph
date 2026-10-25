# ADR-0050 — Journey identity and tier

- **Status:** Proposed for W11 (owner review; design review by Codex)
- **Date:** 2026-10-25
- **Deciders:** Cartograph owner; implementation agents (Claude, Codex)

## Context

W11 in [ROADMAP.md](../ROADMAP.md) recovers journeys: what a user does in a
client, step by step, to reach a goal, and which server behavior each step
reaches. ADR-0036 decision 2 makes a journey a projection with content
acceptance. Decision 3 makes "calling a hop sequence a journey" a
rule-derived interpretation at T2, InferredWeak. This ADR fixes what a
journey is built from, how it is identified, and what tier each layer
carries, before any producer exists.

Today's facts:

- **Screens.** `Screen` nodes (`screen:{repo}@{route}`) come only from React
  Router. Each is a flow trigger, and `RENDERS` links it to its component.
- **No navigation facts.** Nothing records that one screen leads to another.
- **Client calls.** `FETCHES` facts exist, and #737 stitches the browser
  extension's and Android's calls to server endpoints through the system
  manifest (`cartograph.system.toml`).
- **Server side.** Flows, their test references (W10d) and test drift (W10b)
  exist.

SimpleLogin's two clients show both navigation styles a first producer must
read:

- **Browser extension (Vue Router).** `Navigation.getRoutes` maps literal
  paths to components (`/main` → `Main`, `/new-alias-result/` →
  `NewAliasResult`, eight routes). Components move between them with
  `Navigation.navigateTo(Navigation.PATH.X)`, a wrapper over `router.push`.
- **Android (Navigation 3).** `AppRoot` registers each screen with
  `entry<XDestination> { … }` inside `NavDisplay`'s `entryProvider` (13
  entries). `AppRootViewModel` navigates with `backStack.add(XDestination)`,
  called from the callbacks that `AppRoot` passes into each screen's
  composable.

## Decision

1. **Two layers with different tiers.**
   - **Navigation facts are T0.** A screen and a navigation between screens
     are deterministic facts under a named, versioned extraction rule.
     Producers recognize import-proven framework shapes, the way endpoints
     and test clients already are.
   - **A journey is a T2 candidate.** A rule-derived interpretation over
     those facts, InferredWeak, with the rule and version recorded as its
     method (ADR-0036 decision 3). It is never written through the T0
     writer. It is InferredStrong only under a declared independent
     validation contract, and no such contract exists yet.
   - **Names and goals are separate claims.** "Create an alias" is a claim
     about a candidate, not part of it. A human or an expected set supplies
     one, and a model-proposed name is T3/InferredWeak. Human acceptance
     changes review status only.

2. **Screen identity extends the existing scheme.**
   - **Path routers** (React Router, Vue Router, later Next.js): the id stays
     `screen:{repo}@{route}`, where `route` is the declared literal path.
   - **Key-based routers** (Navigation 3, Compose `NavHost` routes, Android
     activities): the route is `{router}:{key}`, for example
     `navigation3:io.simplelogin.android.root.CreateAliasDestination` or
     `activity:io.simplelogin.android.MainActivity`.
   - **Props.** Every Screen carries `router`. `RENDERS` keeps linking a
     screen to the component, composable or activity that draws it.
   - **Computed routes.** A computed path or key is a Gap naming the
     registration site, never a guessed screen.

3. **Navigation is a `NAVIGATES_TO` edge from code to a screen.**
   - **The edge.** It runs from the Symbol whose body performs the navigation
     to the target Screen, citing the call. Examples are a proven
     `router.push(literal)`, a proven wrapper such as `navigateTo(PATH.X)`
     resolved through a literal constant table, `backStack.add(XDestination)`
     (directly, or through a helper in the same class whose builder argument
     constructs the destination, as SimpleLogin's
     `addToBackStackWithApiKey { CreateAliasDestination(…) }` does) and
     `startActivity(Intent(this, X::class.java))`.
   - **Where it comes from.** The source screen is not stored on the edge. A
     journey rule derives it from `RENDERS` and calls.
   - **Callbacks.** For callback wiring such as Navigation 3's, a navigation
     reached from a lambda or function reference passed to the composable
     inside `entry<X>` (for example `onCreateAlias = ::showCreateAliasScreen`)
     is attributed to screen X under an `entry_scoped` link, which is a
     recorded assumption.
   - **Gaps.** A computed target, a router value that is not proven, or a
     wrapper whose table entry is not literal is a Gap linked by
     `NAVIGATES_TO`, naming the site.
   - **Branches and back.** A navigation inside a condition is recorded with
     `conditional: true`. The condition is not interpreted as a business
     rule. Back navigation (`router.back()`, `removeLast`) is recorded as
     `kind: "back"`, not as an edge to a guessed screen.

4. **A journey candidate is one bounded, goal-bearing path.** Rule
   `journey-candidate@1`:
   - **Path.** Start at an entry screen: the router's root path or initial
     destination, or another trigger already recognized (deep link, popup,
     context menu). Follow `NAVIGATES_TO` through each screen's rendered
     code (`RENDERS`, then calls within the client repository, bounded as
     flows are).
   - **Goal.** Stop at the first step whose code issues a state-changing
     client call (`POST`, `PUT`, `PATCH` or `DELETE`) that #737 stitches to
     a recovered Endpoint. Include the screen that same code navigates to
     afterward, if any, as the result step. When the endpoint's server flow
     has recovered data writes, a goal must reach one. Otherwise the HTTP
     method decides, and the candidate records `goal_basis: "http_method"`.
     For example, SimpleLogin lists aliases with `POST /api/v2/aliases`, so
     a method-only goal can stop at a read.
   - **Steps.** Each candidate lists its steps in order. A step is a screen,
     the navigation edge taken, and the client calls made there with their
     stitched endpoints. Each endpoint's server flow and its W10 test
     references are attached by reference.
   - **Limits.** Paths are simple (no revisited screen) and at most six
     screens long. Each (entry screen, goal endpoint) pair gets one
     candidate, the shortest, with ties broken by step ids. Any further
     paths are counted, not enumerated.
   - **Recorded limits.** Truncation, a conditional step and an unresolved
     call are recorded on the candidate, never dropped.
   - **Confidence.** The candidate's confidence is no higher than its
     weakest step, stitch or attached fact.

5. **Journey identity is content-addressed over its steps.**
   - **The id.** `journey:{repo}@{rule}:{digest}`, where `digest` hashes the
     canonical ordered step identities (screen ids, navigation evidence
     identities and goal endpoint ids) together with the rule version.
   - **Stability.** A re-ingest that yields the same path keeps the id. A
     different path is a different candidate, so a rejection never carries
     over to it (as with test drift findings).
   - **Labels.** A name or goal claim attaches to the id and becomes stale
     when the id disappears. Display labels come from facts only, for
     example `Main → POST /api/v2/alias/custom/new → NewAliasResult`.

6. **Content acceptance uses an expected set frozen before the run.**
   - **The expected set.** For the W11 gate, the journey is "create an alias
     from the extension". Its expected screens, client calls and server
     endpoints are written from SimpleLogin's public help, without looking
     at Cartograph's output, and frozen with the owner's go before the run.
   - **Labels.** Each expectation is labeled implemented, documented intent,
     inferred or unknown (ADR-0036 decision 2).
   - **Scoring.** The candidate is scored step by step, and missing or extra
     steps are reported by name.
   - **Partial journeys.** A partial journey is reported as partial,
     together with the step it is missing. A documented alternate path never
     becomes an implemented-path claim.

## Consequences

- **SPEC-00.** Section 5.1 gains `NAVIGATES_TO`, the key-based route forms
  and the `Journey` claim kind in a follow-up spec change. The W11 story
  reserves its acceptance criteria and test ids when its first producer
  lands. This ADR changes no shipped behavior, so it has no changeset.
- **Producers come first.** The order is Vue Router routes and the
  `navigateTo` wrapper (extension), then Navigation 3 entries and back-stack
  adds (Android), then the journey rule. Each producer reports its coverage
  per framework in the W9 matrix.
- **What journeys feed.** W12's journeys layer and W6 work packets read
  candidates, never names, as facts. A packet may propose a test for a
  journey step whose endpoint has no recovered test reference.
- **Review.** The file stays owner-reviewed (ADR-0029). Codex reviews the
  design, as W11 states.
- **Relations.** This extends ADR-0036 decisions 2 and 3 and supersedes
  nothing.

## Alternatives

1. **Journeys from server flows alone:** rejected. A server flow starts at
   an endpoint or schedule. Without client navigation, it cannot say what
   the user did to reach it, and that is what the journey artifact is for.
2. **Model-proposed journeys first, checked against the code afterward:**
   rejected by ADR-0036 decision 1. The navigation facts are deterministic
   and available, so interpretation waits for them.
3. **Journeys as T0 facts:** rejected. Choosing a path and calling it a
   journey is an interpretation, even when every step is a fact.
4. **Every path through the screen graph:** rejected. Even SimpleLogin's
   small graphs grow combinatorially once back edges and conditionals
   exist. One shortest candidate per entry and goal, with the remainder
   counted, keeps the artifact reviewable.
5. **Ids from journey names, or from the entry and goal pair:** rejected.
   Names are claims. An entry and goal id would let a curation of one path
   silently apply to a different path to the same goal.
6. **Naming journeys from UI strings (button labels):** deferred. Labels
   may be recorded later as evidence on a step, but a name stays a separate
   claim.
