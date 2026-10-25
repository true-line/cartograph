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
   - **Callbacks are not T0 reachability.** In callback wiring such as
     Navigation 3's, `entry<X>` passes a lambda or function reference to a
     composable (for example `onCreateAlias = ::showCreateAliasScreen`). The
     T0 layer records only what is proven: the registration site, and the
     `NAVIGATES_TO` edge from the callback's Symbol. It never writes an edge
     from screen X to the callback, because passing a callback does not prove
     that the composable calls it, or on which event or branch.
   - **The assumption lives in the candidate.** A journey candidate that
     crosses from screen X to a callback passed inside `entry<X>` records an
     `entry_callback` assumption: the registration evidence, the parameter
     it was passed as, and the limitation "invocation not proven". The
     candidate is then at most InferredWeak, and it says it is assumed.
   - **Examples.** A callback passed under one parameter to two entries'
     composables gives each entry its own assumption; neither entry owns the
     navigation as a fact. A callback whose parameter the composable never
     reads is not crossed, and the step is a Gap naming the unused
     parameter. A callback forwarded to a nested composable is crossed only
     when each forwarding is a direct pass of the same parameter, and each
     hop is listed in the assumption.
   - **Gaps.** A computed target, a router value that is not proven, or a
     wrapper whose table entry is not literal is a Gap linked by
     `NAVIGATES_TO`, naming the site.
   - **Branches and back.** A navigation inside a condition is recorded with
     `conditional: true`. The condition is not interpreted as a business
     rule. Back navigation (`router.back()`, `removeLast`) is recorded as
     `kind: "back"`, not as an edge to a guessed screen.
   - **One edge, every site.** The store keeps one edge per (source,
     target, label) (SPEC-00 §5), so one Symbol's several navigations to
     one Screen are one `NAVIGATES_TO` edge. The edge lists each call in
     `sites`, in source order, with that site's evidence and its own
     `conditional`. The edge's `conditional` is true only when every site
     is conditional. A journey step names the site it took, so no
     occurrence is lost.

4. **A journey candidate is one bounded, goal-bearing path.** Rule
   `journey-candidate@1`:
   - **Path.** Start at an entry screen: the router's root path or initial
     destination, or another trigger already recognized (deep link, popup,
     context menu). Follow `NAVIGATES_TO` through each screen's rendered
     code (`RENDERS`, then calls within the client repository, bounded as
     flows are).
   - **Goal.** A goal is a step whose code makes a client call that #737
     stitches to a recovered Endpoint, under one of two bases, in this order
     of precedence:

     - `goal_basis: "observed_write"`: the Endpoint's server flow reaches a
       recovered data write. Any HTTP method qualifies.
     - `goal_basis: "method_assumed"`: the method is `POST`, `PUT`, `PATCH`
       or `DELETE`, and no recovered write is reached. The candidate also
       records `write_coverage: "unknown"`, because a missing write may be
       a write the server pass did not recover. Such a candidate is an
       assumption, never a proven business goal.

     The search stops at the first observed-write goal on a path. A
     method-assumed step before it is recorded on the candidate but does not
     end the path. A path with no observed write ends at its first
     method-assumed step. SimpleLogin lists aliases with
     `POST /api/v2/aliases`. That is the fixture for this rule: with no
     recovered write behind it, it may only be a method-assumed goal.
   - **Result step.** The screen reached after the goal is included only
     when ordering is proven. The navigation must be in the same function
     body as the goal call, after it in a straight line or in a branch on
     that call's result (for example `if (res.status === 201)` after an
     `await`). A navigation that is merely reachable, in another callback,
     or before the call is not a result step. The candidate then records
     `result: "unordered"`.
   - **Steps.** Each candidate lists its steps in order. A step is a screen,
     the navigation edge taken, and the client calls made there with their
     stitched endpoints. Each endpoint's server flow and its W10 test
     references are attached by reference.
   - **Dependency injection is an assumption.** A call on an interface is a
     T0 fact about the interface member, not about which implementation runs.
     A Kotlin `@Binds` in a `SingletonComponent` Hilt module records an
     InferredStrong `CALLS` hop from each interface member to the bound
     implementation's member, citing the binding. A candidate crosses that
     hop only as a `hilt_dispatch` assumption naming the interface member,
     the implementation and the binding. Everything reached from there
     inherits it, so the candidate is at most InferredWeak. A proven call to
     the implementation dominates. Any other component, a qualifier, a
     multibinding, a generic key or a second binding for the same interface
     records no hop.
   - **Limits.** Paths are simple (no revisited screen) and at most six
     screens long. The search admits a fixed work budget per entry screen:
     expanded states, client calls read per screen, and retained
     candidates. The rule version records the budget. Each (entry screen,
     goal endpoint) pair gets one candidate, the shortest found, with ties
     broken by step ids.
   - **Alternatives.** Further paths are counted, not enumerated. When the
     budget ran out, the count is a lower bound and the candidate says
     `alternatives_complete: false`. The shortest path found is then the
     shortest within the budget, not a proven shortest path.
   - **Recorded limits.** Truncation, a conditional step and an unresolved
     call are recorded on the candidate, never dropped.
   - **Confidence.** The candidate's confidence is no higher than its
     weakest step, stitch or attached fact.

5. **Journey identity is the path; freshness is a separate fingerprint.**
   - **The id.** `journey:{repo}@{rule}:{digest}`. `digest` hashes the rule
     version and, in order, each step's screen id, the navigation taken as
     (source Symbol id, target Screen id, kind), and the goal Endpoint id.
     No commit SHA, byte offset or line enters it, so an unrelated re-ingest
     keeps the id.
   - **Stability.** A re-ingest that yields the same path keeps the id. A
     different path is a different candidate, so a rejection never carries
     over to it (as with test drift findings).
   - **The support fingerprint.** Each candidate also carries
     `support_fingerprint`. It hashes the content of everything the
     candidate rests on: each step's navigation and client-call facts, the
     stitches, the goal basis and its write facts, the attached flows and
     test references, the recorded assumptions and the truncation. Unlike
     the id, it changes when that support changes and the path does not.
   - **Labels and acceptance.** A name, goal claim or human acceptance
     attaches to the id and records the fingerprint it was made against.
     It becomes stale when the id disappears. When the id survives but the
     fingerprint changes, it is marked stale against the new support, never
     silently carried over. Packets and reads (W8a, W6) show the same
     revision and support qualification. Display labels come from facts
     only, for example `Main → POST /api/v2/alias/custom/new →
     NewAliasResult`.

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
  journey step whose endpoint has no recovered test reference. "No
  recovered test reference" stays an evidence gap, not proof that no test
  exists. Packets remain proposals, and carry the candidate's revision,
  support fingerprint, assumptions and omissions as UI, export and MCP
  reads do.
- **Review.** The file stays owner-reviewed (ADR-0029). Codex reviews the
  design, as W11 states.
- **Relations.** This extends ADR-0036 decisions 2 and 3 and supersedes
  nothing.

## Alternatives

1. **Journeys from server flows alone, or proposed by a model first:**
   rejected. A server flow starts at an endpoint or schedule, so it cannot
   say what the user did to reach it, and that is what the journey artifact
   is for. Proposing journeys before the deterministic navigation facts
   exist reverses ADR-0036 decision 1.
2. **Journeys as T0 facts, or every path through the screen graph:**
   rejected. Choosing a path and calling it a journey is an interpretation,
   even when every step is a fact. Enumerating every path grows
   combinatorially once back edges and conditionals exist. One bounded
   shortest candidate per entry and goal, with the rest counted, keeps the
   artifact reviewable.
3. **Ids from journey names, from the entry and goal pair, or from UI
   strings:** rejected. Names are claims. An entry and goal id would let a
   curation of one path silently apply to a different path to the same
   goal. UI strings such as button labels may later be recorded as evidence
   on a step, but a name stays a separate claim.
