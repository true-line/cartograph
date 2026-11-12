# SPEC-13 — Calls and publishes that leave the system boundary

- **Status:** Proposed for [#1788](https://github.com/true-line/cartograph/issues/1788); not implemented.
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
| Outbound HTTP call that matched no endpoint | `Gap` `gap:fetch:…` reached by `FETCHES`, with `request {method, url, url_source}` when the URL resolved, and `reason` / `t0_reason` |
| Observed client | `Resource` with `observed: true`, `kind` `http-api`, `sdk-client`, `auth-provider` or `browser-storage`, reached by `USES` |
| Channel | `Channel` `chan:{kind}:{identity}` with `PUBLISHES`, `SUBSCRIBES`, and `BACKS` from a resource |
| Declared bindings | The system manifest's service base URLs and gateway routes, as cross-repository HTTP binding reads them |
| Flows | `flowtracer` flows; a flow does not continue past a Gap |
| Data entities | `DataEntity` with `READS` / `WRITES` from a Symbol |

## Classification

`boundary(target) -> Inside | Outside | Unclassified(reason) | NotACrossing`
is one pure function. The first matching row applies.

| Target | Condition | Class |
|---|---|---|
| Fetch Gap | Binding evidence names a registered repository (no endpoint matched, or ambiguous) | not a crossing; counted as "inside, unresolved" |
| Fetch Gap | `request.url` has a literal scheme and host, and no declared binding covers it | outside |
| Fetch Gap | URL without a host (same origin) | not a crossing; it is an unresolved internal call |
| Fetch Gap | No `request`, or host not literal | unclassified: host not known from source |
| Observed `http-api` | Literal origin covered by a declared binding | inside |
| Observed `http-api` | Literal origin, no binding | outside |
| Observed `http-api` | Origin from configuration or run time | unclassified: host not known from source |
| Observed `sdk-client`, `auth-provider` | Any | unclassified: the registries do not record who operates this provider |
| Observed `browser-storage` | Any | not a crossing |
| Channel, kind `inproc-event` | Any | not a crossing |
| Channel with a publisher in a registered repository | A recovered subscriber, or a `BACKS` resource of a registered repository | inside |
| Channel with a publisher in a registered repository | Neither | unclassified: no subscriber or backing resource recovered |
| Channel with no recovered publisher | Any | not listed (inbound) |

A host is compared as scheme, host and port, exactly as written. No DNS, no
suffix list and no allowlist is used.

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
2. Counts: outside targets, unclassified targets, inside-unresolved calls.
3. **External services** (D7 §7.6): one row per outside target.
4. **External data flows** (D6 §6.5): one row per flow and outside target,
   with the entry point and the entities.
5. **Unclassified**: one row per target with its reason.

`verified-only` omits a row whose fact is InferredWeak and states how many it
left out; `best-effort` includes it, marked (R-INT-5). The same graph gives
the same bytes. Citations go through the fact index (ADR-0058). Work packets
refuse the id until a decision says otherwise.

## Acceptance evidence

On a two-repository fixture with a declared binding: a call from one
repository into the other is absent; a call to a third-party host is present
with its flow, span and entities; a flow that breaks before the outbound call
is absent and remains partial; a run-time host and an SDK client are
unclassified with their reasons; a channel with a subscriber is absent and
one without is unclassified.

## Open items

- Operators of SDK providers need a registry field before an SDK client can
  be outside.
- Inbound channels and webhooks from outside are not listed.
- The MCP read of this artifact follows the catalogue's default; no separate
  decision is made here.
