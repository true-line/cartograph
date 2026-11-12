# ADR-0063 — The system boundary: registered sources are inside, and nothing is called outside without a literal host

- **Status:** Proposed; owner review
- **Date:** 2026-11-12
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1788; #1745; US-0015 / AC-0660; `docs/DOCUMENT-CATALOGUE.md` D6 §6.5, D7 §7.6; [SPEC-13](../SPEC-13_external-boundary.md); R-INT-2, R-INT-4, R-INT-5

## Context

D6 §6.5 "External data flows" and D7 §7.6 "Dependencies on external
services" need one input: the calls and publishes that leave the system. The
graph has no "external" fact. An outbound HTTP call that matches no recovered
endpoint is a `Gap` (`gap:fetch:`), with the resolved request where the URL
is known. A client of a hosted service is an observed `Resource` reached by
`USES`. A message leaves through a global `Channel`. No predicate says which
of these is outside, and a wrong boundary would read as a complete list.

## Decision

1. **Inside** is what the registered sources establish: a recovered endpoint
   of a registered repository, a host that a declared service or gateway
   binding of the system manifest covers, a resource a registered repository
   declares or its observed state reports, and a channel with a recovered
   subscriber or a backing resource in a registered repository.
2. **Outside** is claimed only from a literal: an outbound HTTP call, or an
   observed HTTP client, whose scheme and host are written in the source and
   are covered by no declared binding. A service the registered sources do
   not contain is outside the documented system by definition, including an
   unregistered sibling service.
3. **Unclassified** is everything else that leaves a registered repository's
   code and is not proven inside: a host computed at run time or read from
   configuration, a client of a provider whose operator the registries do not
   record, a channel with a publisher and neither a subscriber nor a backing
   resource. It is listed with the reason and is never counted as outside or
   as inside.
4. **Not a boundary crossing:** a call that binding evidence places in a
   registered repository but that matches no endpoint there (it stays a Gap
   and is counted, not listed as a target), an in-process event channel,
   browser storage, and an imported package (a build dependency, not a call).
5. The predicate is one function in `crates/spec`. It computes; it stores
   nothing and changes no fact, tier or hash. The projection never upgrades a
   Gap: a row carries the tier and confidence of the fact that holds its
   literal.

## Consequences

- The outside list is a lower bound. The document says so and gives the
  unclassified count beside it.
- Without a system manifest, a call to another registered repository by
  absolute URL is listed as outside until a binding declares its host. The
  document names that limit.
- Declared bindings are not stored in the graph. Where a binding pass ran, the
  kept Gap does not record whether a binding covers its host, so a literal
  third-party call there is unclassified until that outcome is a field on the
  fact (a graph-hash change, decided separately).
- A channel is never listed as outside: the graph cannot show who owns a
  queue or topic. It is inside or unclassified.
- Provider operators (which SDK clients are third-party services) are a
  registry addition, not part of this decision.

## Alternatives

1. **Treat every unresolved outbound call as external.** Lists internal calls
   the tracer failed to resolve as third-party dependencies.
2. **A host allowlist of known third parties.** A list the tool would have to
   maintain and that guesses for hosts not on it.
3. **Store boundary facts at ingest.** Changes fact identity and the graph
   hash for a classification that depends on which sources are registered.
