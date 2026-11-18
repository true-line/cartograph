# SPEC-13 — Calls and publishes that leave the system boundary

- **Status:** Implemented for [#1788](https://github.com/true-line/cartograph/issues/1788); the binding outcome is recorded on the evaluated facts for [#1843](https://github.com/true-line/cartograph/issues/1843) (see "Binding outcome"); owner review of ADR-0063 pending.
- **Date:** 2026-11-12
- **Scope:** US-0015 / AC-0660; `docs/DOCUMENT-CATALOGUE.md` D6 §6.5 and D7 §7.6.
- **Decision:** [ADR-0063](adr/ADR-0063-system-boundary.md).

## Outcome and boundary

One deterministic projection, `external.md`, lists what the registered
sources call or publish to outside themselves, which flows reach each target,
and which data entities those flows read or write. It reads T0 and T1 facts
only and writes none.

Not in this specification: the sensitivity of data; what a request body
contains; inbound traffic from outside; network egress with no code or
infrastructure evidence; package dependencies; operators of SDK providers.

## Inputs

| Fact | Shape today |
|---|---|
| Outbound HTTP call that matched no endpoint | `Gap` `gap:fetch:…` reached by `FETCHES`, with `request {method, url, url_source}` when the URL resolved, and `reason` / `t0_reason`; where the binding pass evaluated it, `binding_state` and the `binding` citations of the bindings that name its origin |
| Observed client | `Resource` with `observed: true`, `kind` `http-api`, `sdk-client`, `auth-provider` or `browser-storage`, reached by `USES`; an `http-api` client of a declared origin carries those `binding` citations |
| Channel | `Channel` `chan:{kind}:{identity}` with `PUBLISHES`, `SUBSCRIBES`, and `BACKS` from a resource |
| Declared bindings | The manifest is not stored. The compiler knows a bound origin from the binding citations the binding pass left on the facts it evaluated: joined `FETCHES` relations, kept and ambiguous fetch Gaps, and observed `http-api` clients |
| Flows | `flowtracer` flows; a flow does not continue past a Gap |
| Data entities | `DataEntity` with `READS` / `WRITES` from a Symbol |

## Classification

`boundary(target) -> Inside | Outside | Unclassified(reason) | NotACrossing`
is one pure function. The first matching row applies.

| Target | Condition | Class |
|---|---|---|
| Fetch Gap | Ambiguous binding (`candidates`), or `binding_state` `origin_declared`, `names_repository` or `ambiguous`, or its origin is one a binding citation names | not a crossing; counted as "inside, unresolved" |
| Fetch Gap | URL without a host (same origin) | not a crossing; counted as an unresolved same-origin call |
| Fetch Gap | Host without a scheme | unclassified: scheme not written in source |
| Fetch Gap | No `request`, or host not literal (computed, or read from configuration, including an environment file in the repository) | unclassified: host not known from source |
| Fetch Gap | Literal origin, `binding_state` `none` | outside |
| Fetch Gap | Literal origin, kept by a binding pass that recorded no state (`t0_reason` present, no `binding_state`) | unclassified: the fact does not record whether a declared binding covers its host |
| Fetch Gap | Literal origin, computed method, no `binding_state` | unclassified: no binding pass recorded what the bindings say about its origin |
| Fetch Gap | Literal origin, no binding pass ran over it | outside |
| Observed `http-api` | Literal origin covered by a declared binding | inside |
| Observed `http-api` | Literal origin, no binding | outside |
| Observed `http-api` | Origin from configuration or run time | unclassified: host not known from source |
| Observed `sdk-client`, `auth-provider` | Any | unclassified: the registries do not record who operates this provider |
| Observed `browser-storage` | Any | not a crossing |
| Channel, kind `inproc-event` | Any | not a crossing |
| Channel with a publisher in a registered repository | A recovered subscriber, or a `BACKS` resource of a registered repository | inside |
| Channel with a publisher in a registered repository | Neither | unclassified: no subscriber or backing resource recovered |
| Channel with no recovered publisher | Any | not listed (inbound) |

A host is compared as scheme, host and port, with scheme and host
lower-cased and user information dropped, as the binding pass compares them.
No DNS, no suffix list and no allowlist is used.

### Binding outcome

The binding pass records what the declared bindings say about each fact it
evaluates (#1843). The projection reads those fields and never a sentence.

| Fact | Recorded |
|---|---|
| Kept fetch Gap and its Gap edge | `binding_state`: `none` (no declared binding names the origin), `origin_declared` (a binding names the origin and none covers the URL, or the method is computed), `names_repository` (a binding covers the URL and no endpoint there matches) or `ambiguous` (`candidates`) |
| Kept fetch Gap | `binding`: the citations of the bindings that name its origin, for `origin_declared` and `names_repository` |
| Observed `http-api` client | `binding`: the citations of the bindings that name its origin |

A call with a computed method is matched to no endpoint; the pass records
the state of its origin and leaves its reason unchanged. So in a system with
declared bindings a literal third-party call is outside, a call a binding
places in a registered repository is counted as inside, unresolved, and an
observed HTTP client of a declared host is not listed as outside.

The fields change the bytes of the facts that carry them, and so the graph
hash of a system with declared bindings, on its next ingest. The facts belong
to the repository that made the call, so a re-ingest or a forget of that
repository replaces or removes them as it does the Gap itself. A graph
ingested before the state was recorded keeps the earlier result: a call the
pass kept is unclassified with its sentence, and an observed client of a
declared host that no joined or ambiguous call names is listed as outside.
A system with no declared binding has no binding pass, so a literal call
with a computed method stays unclassified there.

One limit remains from bindings not being stored: an absolute URL whose path
matches an endpoint of the calling repository is resolved there by the
per-repository pass when no binding covers its host, and never appears.

## Targets and rows

Outside HTTP targets are grouped by origin. One origin is one target, with
every fetch Gap and every observed `http-api` use of it as its sites; a call
that appears as both is one site.

Per target: the class, the origin or channel identity, each site with its
source span, method and path where recovered, and the tier and confidence of
the fact that holds the literal. A row never states a higher confidence than
that fact.

**Flows.** A flow reaches a target when one of its hops ends at the target's
Gap or Channel. Because a flow stops at the first Gap on a branch, a flow
that breaks before the outbound call has no such hop and is not listed; it
stays partial in the gap register (R-INT-4). An observed use is not a flow
hop: its flows are those with a non-Gap hop at the using Symbol, and where
the use is not at a Symbol on any flow the row says "no flow recovered
through this site".

**Data entities.** For each flow that reaches a target, the entities with a
`READS` or `WRITES` edge from a Symbol on a non-Gap hop of that flow. The
column is headed "entities read or written on these flows". It does not say
the entity is sent. Where there is none, "none recovered".

## The document

`external.md`, artifact id `external`, in both export modes:

1. A statement of the boundary and its limits: the outside list is a lower
   bound; without declared bindings, a call to another registered repository
   by absolute URL is listed as outside.
2. Counts: outside targets, unclassified targets, inside-unresolved calls,
   unresolved same-origin calls, and published channels that are inside.
3. **External services** (D7 §7.6): one row per outside target.
4. **External data flows** (D6 §6.5): one row per flow and outside target,
   with the entry point and the entities.
5. **Unclassified**: one row per target with its reason.

`verified-only` omits a row whose fact is InferredWeak and states how many it
left out; `best-effort` includes it, marked (R-INT-5). The same graph gives
the same bytes. Citations go through the fact index (ADR-0058). Work packets
refuse the id until a decision says otherwise.

## Acceptance evidence

On a fixture with no binding pass: a call to a third-party host is outside
with its flow, span and entities, and a flow that breaks before the outbound
call is absent and remains partial. On a two-repository fixture with a
declared binding: a call from one repository into the other is absent; a
third-party call is outside with its flow and span; a call a binding places
in the other repository is counted as inside, unresolved; an observed HTTP
client of a declared host is not listed; a run-time host and an SDK client
are unclassified with their reasons; a channel with a subscriber is absent
and one without is unclassified. On the same fixture in the shape stored
before the binding state was recorded, the third-party call is unclassified
with the recorded sentence.

## Open items

- A system with no declared binding has no binding pass, so a literal call
  with a computed method is unclassified there.
- A second call from one Symbol whose `USES` relation follows an identity to
  its definition is treated as the same site.

- Operators of SDK providers need a registry field before an SDK client can
  be outside.
- Inbound channels and webhooks from outside are not listed.
- The MCP read of this artifact follows the catalogue's default; no separate
  decision is made here.
