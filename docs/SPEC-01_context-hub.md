# SPEC-01 — Domain context hub and agent interoperability

Status: implementation in progress. Parent issues: #250, #255; first slice: #380.
Decision: [ADR-0019](adr/ADR-0019-domain-context-and-agent-interoperability.md).
This specification extends SPEC-00; the provenance and escalation invariants remain binding.

## Product contract

Cartograph is a persistent, local-first context hub composed of business domains.
Each domain connects features, business rules, flows, data, designs, decisions,
tests, and gaps to source evidence. Humans and agents use the same context.
Language and infrastructure adapters supply evidence; folders and languages do
not by themselves establish business-domain boundaries.

The first user is the owner investigating and modernizing existing repositories.
The initial external proving ground is Vendure's cart-readiness transition, with
a pinned revision, explicit configuration, and an independently reviewed expected
set. Support for arbitrary repositories is an eventual coverage objective, not a
claim that every framework or business rule is already recovered.

## Knowledge and review

Keep four meanings distinct: implemented behavior, documented intent, inferred
interpretation, and proposed future design. A source document proves that an
intention was written; it does not prove the code implements it. Missing design
evidence means unknown, not proof that no design exists. Feature inventories name
their scope, source revision, uncovered portions, and confidence.

Domain/feature membership and proposed rules require typed, cited relationships.
T3 can propose interpretations, designs, and changes; human acceptance records a
review decision without upgrading their producing tier or confidence ceiling.
Accepted proposals must enter a shared curated projection used by the UI, exports,
and agent reads. Re-ingestion marks unsupported or changed citations stale and
requires reconciliation. The recovered graph remains separately addressable.

## Shared reads — first implementation slice

The `context-hub` crate provides an immutable recovered-graph snapshot and a
transport-independent query contract. The app exposes it through a Tauri command
on a blocking worker, with the graph lock held only while copying the graph.
Nodes and edges are copied within one SQLite read transaction so another app
process cannot interleave a commit and create a snapshot that never existed.
It makes no model, network, filesystem-evidence, or target-write calls.

- A snapshot identifies the canonical complete node/edge content with a versioned
  content hash. Input ordering does not change it; changing any property does.
  Duplicate node identities or duplicate directed edge identities are errors.
- Fact references are a tagged node id or a typed `(source, label, destination)`
  edge identity, not an ambiguous concatenated string. Responses identify the
  `recovered_graph` view; they do not claim to contain accepted proposals yet.
- Queries select all facts, one exact node (`{"type":"node","id":"opaque node ID"}`),
  or a bounded undirected neighborhood of a known node,
  and optionally filter by fact kind and exact labels. Nodes sort by id, edges by
  `(source, label, destination)`, with nodes preceding edges. A neighborhood
  includes induced edges; the exact-node scope excludes all edges, including self-loops.
  Missing node IDs or anchors are errors, not fabricated facts. Exact-node support
  in this core/direct-query contract does not alter historical specialist schemas.
- A request has item and serialized-response byte budgets, with hard caps of
  500 facts, 1 MiB, and three neighborhood hops. Zero or excessive limits fail
  explicitly. The service never silently skips an oversized fact to fit later
  facts. If even the next fact and envelope cannot fit, it returns a budget error.
- Paging cursors bind the snapshot and normalized selection. Reusing a cursor
  after source changes or for another selection is an error. Every response
  includes the snapshot id, total selected count, returned facts, and an explicit
  next cursor when more remain. Changing only budgets is allowed between pages.
- Each fact carries validated provenance or an explicit provenance problem and
  `Gap` confidence. Raw `prov` properties are removed from returned properties;
  only validated provenance is exposed as authoritative metadata. Inference never
  becomes confirmed. Evidence references remain available through that metadata.
- Label filtering for domain concepts with no producer returns an empty result;
  the service must not manufacture a domain or feature from directory names.

This content identity is a query consistency contract. It does not fix upstream
location-dependent extraction identities (#342). Rebuilding snapshots per query
is the initial app implementation; reusable snapshot caching/index invalidation
needs a measured large-repository gate before broad rollout.

The Tauri command is `query_context` with a `request` argument. For example:

```json
{
  "request": {
    "scope": { "type": "neighborhood", "anchor": "symbol:example", "hops": 1 },
    "kind": null,
    "labels": [],
    "max_facts": 100,
    "max_bytes": 65536,
    "cursor": null
  }
}
```

Use an actual recovered node identity for the anchor. Continue with the returned
`next_cursor` and the same selection. Typed references, validated provenance,
`confidence_tier`, and `provenance_problem` accompany each fact. Cursor identities
are consistency tokens, not authorization credentials. No MCP endpoint is implied
by the existence of this Tauri command.

### Coverage-qualified reads (AC-0239, #588)

A recovery publishes its repo's coverage inventory (AC-0238) in the same write
transaction as the repo's facts and source bindings. A publication without an
inventory clears the repo's previous one rather than leaving it to describe other
facts. Graph clearing removes every inventory. The record binds the registered
source, the ingest job, the plugin basis (`coverage-config-v1`, the sorted pinned
plugin ids) and the inventory's content hash. Records are validated on write and
hash-checked on read. The `coverage_*` tables must be exactly as created.
`read_snapshot_with_coverage_bounded` admits the graph as `read_snapshot_bounded`
does. It also counts the inventories and measures their stored bytes, both before
any of them is copied (at most 4096 records and 16 MiB).

`query_qualified_context` takes `{"request": {"request": <query request>,
"context_id": null}}`. It reads the graph and every inventory in one
transaction. The response is `{context_id, coverage, response}`, where `response`
is an unchanged `query_context` page. `coverage` has contract
`coverage-qualification@1` and always has `exhaustive: false`. It qualifies every
registered or recorded repo with one of these statuses:

- `recorded`: the registration and plugin basis are current. When the caller
  requests a layer selection, it must match the recorded one as a set. A recorded
  repo carries its commit, inventory hash, `limited`, producer-state counts and
  recorded layer hints.
- `stale`: an inventory exists, but its registration, plugin basis or requested
  layer selection differs, or the repo is no longer registered. Only its hash is
  reported, and nothing is derived from its body.
- `unrecorded`: no inventory exists.

`limited` is true when any producer was excluded or lacked a declared input, or
when any capability is unsupported. A producer with nothing to read does not
limit coverage. Each repo's `compared` field lists what was checked at read time.
Everything else, declared inputs included, is as recorded at ingest and was not
revalidated.

`coverage_id` binds the public block and the recorded basis behind it.
`context_id = content_identity("context-v2", {snapshot_id, coverage_id})`, and
`snapshot_id` keeps its graph-only meaning. The envelope counts inside
`max_bytes`. A qualification that cannot fit fails with the budget error and is
never dropped. A continuation must send the `context_id` it was issued in.
Qualified interpretation versions and qualified discovery build on this read.
`query_context` and its identities are unchanged.

## Coherent legacy graph projections (#388)

The same database-revision guarantee extends to the existing Atlas, specification,
flow/topology, findings, metrics and semantic-preview graph reads. `GraphStore`
requires coherent full and label-filtered snapshots; no backend may implement the
contract as unrelated autocommit reads. Filtering occurs within the read
transaction, preserving existing selections, stable ordering and the treatment of
malformed properties outside the selected labels. `None` selects every label and
an explicitly empty label set selects no facts. Flow tracing, spec compilation,
metrics and other computation operate on owned results after the transaction ends.

Paired graph counts use one coherent count operation without materializing JSON
facts. The source/edge selections and returned count meaning remain unchanged.
Deterministic regression fixtures commit a replacement graph through a second
connection between collection reads and exercise the actual spec/Atlas adapters;
errors must release the transaction so later corrected reads can advance. A narrowly
feature-gated one-shot hook supports test interleaving without runtime app exposure.

This read contract does not make multi-step ingest/reconciliation publication
atomic, prevent competing write plans, or create a transaction across graph,
review, findings and jobs databases. Source-byte verification remains #385.

## MCP ingress, ACP execution

External agents use Cartograph's MCP server to read context and request work.
The app and MCP ingress call one durable task coordinator. That coordinator can
run named internal specialists and launch compatible agent runtimes through an
ACP client. ACP workers can receive Cartograph MCP context tools in their session.
The MCP server and ACP client are separate protocol adapters, not separate copies
of domain state or orchestration policy.

Tasks bind a project, graph revision, scope, agent identity/version, permitted
tools, tier ceiling, input references, budget, origin, and parent task. Starting
returns a durable id promptly. Progress, cancellation, failure, proposals, and
results remain queryable after reconnect/restart. Unknown runtime outcomes after
a crash require reconciliation rather than automatic duplicate execution.

Negotiate and pin protocol/runtime capabilities. MCP Tasks support is optional;
explicit start/status/result/cancel tools provide the compatibility baseline.
ACP permissions are not a filesystem sandbox. The coordinator enforces tool
allowlists and per-tier egress, and the runtime boundary must enforce read-only
access to source plus explicitly scoped output storage. No cloud launch under
local-only policy. No permissions accepted automatically from repository text.
Parent ancestry, delegation-depth limits, deduplication, and shared budgets stop
MCP → ACP → MCP delegation loops and unbounded recursive work.

## Architecture evaluation and modernization

Evaluate a selected feature or domain. Compute structural evidence deterministically
(size, dependencies, cycles, fan-in/out, cohesion proxies and change history when
available). Specialists interpret the measurements against responsibilities,
documented intent, and exceptions. A large file alone is not a god-file finding.
Each finding states its evidence, scope, uncertainty, impact, and proposed remedy.
Missing dependencies or history reduce coverage explicitly.

Modernization connects recovered behavior to approved future design and executable
acceptance criteria. An external coding agent can implement an approved change
in a separately authorized checkout; Cartograph re-ingests it to compare behavior
and intent. This architecture does not authorize target writes or automatic code
generation in the initial implementation.

## Delivery gates

| Gate | Observable result | Current state |
|---|---|---|
| H1 Shared reads | App/core contract passes revision, budget, provenance and paging tests | Implemented in #380/#382; merged foundation CI passed |
| H2 Domain truth | Pinned Vendure feature yields cited rules, feature membership and honest coverage against independent expected cases | Source observations in #381; [baseline](evals/vendure-cart-readiness.baseline.md) records 0/8 complete rules and 41 unexecuted cases; semantic recovery remains open |
| H3 Curated context | Accepted proposals appear consistently in UI, exports and agent context; stale evidence reconciles | Durable staging prerequisite in #384 ([SPEC-03](SPEC-03_proposal-staging.md)); source capture #385 begins with the acquisition/storage core #387 ([SPEC-04](SPEC-04_source-capture.md)); host source registration #392 ([SPEC-05](SPEC-05_registered-sources.md)); primary-source producer receipts and inspection #395 ([SPEC-06](SPEC-06_primary-source-receipts.md)) are in progress; complete input closure, shared projection and freshness reconciliation remain open |
| H4 Agent investigation | Durable specialist task reads context and returns cited, reviewable findings in app | In progress under #256: #404 specifies the app coordinator and specialists in [SPEC-10](SPEC-10_specialist-investigations.md); real MCP cross-ingress acceptance remains required and unexecuted |
| H5 MCP + ACP | A real external MCP caller starts an ACP investigation, observes progress, cancels/reconnects, and retrieves its result | Planned; protocol adapters not shipped by H1 |
| H6 Feature evaluation | Structural measurements and contextual findings distinguish justified large files from responsibility hotspots | Planned |
| H7 Market pilot | New user independently ingests a repo, asks the core domain questions, and completes an evidence-backed improvement with measured time/cost | Planned |

Do not treat these gates as elapsed-time estimates or mark the product ready from
test counts. The first market hypothesis is developers responsible for unfamiliar
or under-documented projects. Validate the workflow with the owner first, then
several external users; packaging, onboarding, recovery latency, credentials,
coverage explanations, and trustworthy output are launch requirements.

Protocol references: [ACP overview](https://agentclientprotocol.com/protocol/v1/overview),
[ACP sessions](https://agentclientprotocol.com/protocol/v1/session-setup),
[MCP Tasks](https://modelcontextprotocol.io/extensions/tasks/overview).

## Packaged read-only client skill (#732)

The desktop bundle includes the canonical `read-cartograph-context` skill tree,
including every referenced document. Settings exposes **Show packaged client
skill** beside client configuration, independently of read-service activation.
The read-only command reports the running app version, verified skill location,
and skill-relative file names with text. It reads only the fixed packaged tree;
file sizes are bounded before reading and bytes must match the sources embedded
at build time. Missing, changed or symlink-substituted descendants yield explicit
unavailability without changing the read service or its configuration.

The interface displays SKILL.md first and reference documents on demand. It
explains that installation and removal use the chosen client's supported workflow
and require a separate user request. Cartograph neither writes client settings
nor installs files into global skill locations. This delivery does not establish
Windows MCP transport support or authorize provider execution.

### Next-family acceptance observations (#1358, AC-0465)

An opt-in acceptance observer records actual prompt9 source, inventory and parent-history hashes, and compares them with saved disclosure6 and policy-qualified request/ledger/contract hashes. Schema-family mismatches fail before delegation. Legacy input6/disclosure4 behavior remains unchanged; all bounds come from the existing family policy. Coverage-detail counts use the tool-action bound (56 for V2, 8 for legacy), rather than the larger completed-attempt bound. The parent-free regression rejects a foreign history hash; actual nonempty saved-parent history observation belongs to the #1354 end-to-end auditor test. This prerequisite neither stages an auditor nor establishes captured-model or semantic acceptance.

### Captured auditor staging (#1359, AC-0466)

A separately frozen child definition binds the exact reviewed saved-parent receipt/result/revision, original graph/context/scope, independently selected source/inventory obligations, compiled auditor@11 identity, provider descriptor and fresh nonce. Provider-free preparation revalidates captured authority and retained targets, checks the existing 16 KiB saved-parent-history bound, and stages one queued child with exclusive frozen artifacts. Before entering the manual preparation side-effect path, the auditor output is validated by the existing preparation root checks: it must be canonical and disjoint from the captured source, native profile and Cartograph checkout. Local review bytes bind content only; external reviewer authentication remains mandatory. Repeated preparation returns the same unexecuted child or refuses changed/ambiguous artifacts. Execution, auditor receipts and terminal reopen remain a separate #1354 increment; no runtime roles, policy ceilings or historical analyst artifacts change.

### Captured auditor execution and receipts (#1354, AC-0463)

Explicit execution revalidates the frozen reviewed parent and child authority, consumes one durable attempt before claiming the queued child, and delegates through the existing worker with the exact frozen provider. Actual prompt9 source/inventory/history observations bind to disclosure6, including the saved parent result ID/body hash. Immutable auditor observations and terminal receipts preserve failed or unknown outcomes; reopen re-derives durable evidence without constructing a provider or dispatching. Reopen rejects observation-count, payload/disclosure, request/context or exact-parent binding mismatches even if both artifact files are rewritten together. Representation quality and missing acquisitions remain diagnostic; no-invocation outcomes require an empty observation set. Preparation remains separately available and provider-free. External review authentication and subsequent auditor semantic review remain operator gates; synthetic host tests do not establish actual-model or native acceptance. Historical analyst artifacts, roles and numeric limits remain unchanged.

## Scoped investigation acquisition basis (#1602, preparation only)

A separately domain-separated host basis binds a valid store instance/generation,
the digest of a normalized exact-node or induced-neighborhood request, the
acquired projection digest and the coverage qualification digest. Its fixed-size
inputs do not include graph bodies. It identifies a requested scoped acquisition;
it does not certify whole-graph completeness, source authorization or semantic
coverage. Paged All is an explicit extent with no aggregate projection digest: each page must bind its own projection and continuation to this immutable basis. Its exhaustion proves raw traversal only, not semantic recovery or absence. Runtime paging still requires a separate implementation. Construction is
host-only and output-only; historical complete snapshot and ledger formats are
unchanged. Runtime activation requires the ledger, evidence and replay integration.

All investigation traversal uses a host-owned, non-deserializable cursor with fixed narrowed page and aggregate limits. It binds every node and edge window to the same acquisition basis/revision. Successful pages charge actual decoded JSON values (including duplicate input property values) and both raw and canonical bytes. A failed acquisition terminates this cursor; outer task admission still accounts for attempted reads and validation. Nodes exhausted does not mean graph exhausted: the edge phase must also finish. Runtime and durable-ledger activation remain separate work.
