# Request examples

These are JavaScript-style examples of MCP tool arguments, not scripts to execute
against arbitrary paths. `call(name, arguments)` means the connected client's
normal MCP tool call returning CallToolResult after JSON-RPC error checking;
resolve any client-specific name prefix from tools/list.
Use the live schemas if they differ. Never invent IDs or rewrite returned targets.

First read capabilities with empty arguments:

```js
const caps = (await call("cartograph_capabilities", {})).structuredContent;
const identity = { session_id: caps.session_id, basis_id: caps.basis_id };
```

Below, `B` is a positive response-byte allowance no greater than the advertised
`caps.limits.max_response_bytes` and the client's receive limit. `N` is the number
of relevant candidates needed, within the live schema limit. `P` is the desired
content-byte allowance, leaving room within `B` for JSON escaping and metadata.
These are per-request selections, not changes to the app's activation policy.

## Node discovery

```js
const query = { by: "text", text: "checkout" }; // replace with the actual question
const selection = {
  query, labels: [], max_candidates: N, max_bytes: B, cursor: null
};
const found = (await call("cartograph_discover", {
  ...identity, request: { context_id: null, request: selection }
})).structuredContent.context;
```

`found.response.candidates` holds bounded references and match reasons;
`found.coverage` qualifies coverage. An exact ID query is
`{by: "identifier", id: knownId}` and an exact name query is
`{by: "name", name: knownName}`. If more candidates are needed and
`found.response.next_cursor` is non-null, repeat the same selection with that
cursor and `context_id: found.context_id`. Do not edit cursor fields.

## Relations of a known node

In a complete session (`caps.relations_access.query_tool_available` is true),
read a found node's neighborhood: the nodes within `hops` (1 to
`caps.relations_access.max_neighborhood_hops`) and the edges among them.

```js
const selection = {
  scope: { type: "neighborhood", anchor: nodeId, hops: 1 },
  kind: null, labels: [], max_facts: N, max_bytes: B, cursor: null
};
const page = (await call("cartograph_query", {
  ...identity, request: { context_id: null, request: selection }
})).structuredContent.context;
```

`page.response.facts` holds nodes and edges with their tier, confidence and
evidence references; an edge's `reference` names its source, label and
destination. Use `kind: "edge"` and exact `labels` such as `["HANDLES"]` to
narrow it, or `{ type: "node", id: nodeId }` for one node. Continue a non-null
`page.response.next_cursor` with the same selection and
`context_id: page.context_id`. A fact the session does not show is absent from
every page, so an empty neighborhood is not proof that a relation does not exist.

In a scoped session the same tool takes the neighborhood fields at the top
level: `{ session_id, session_basis_id, anchor, hops, kind, labels, max_facts,
max_bytes, cursor }`, and the page is at `structuredContent.relations.page`.
Only the neighborhood scope is offered; read one node with
`cartograph_read_fact`. A neighborhood larger than the window limits returns a
limit error, not a partial page: lower `hops` or narrow from a closer anchor.

## Official artifact

```js
const catalog = (await call("cartograph_artifacts", identity)).structuredContent;
// Choose artifactId from catalog.artifacts for the question, e.g. flow-dossiers.
const response = await call("cartograph_read_artifact", {
  ...identity, artifact_id: artifactId, offset: 0,
  max_content_bytes: P, max_bytes: B
});
// Check JSON-RPC errors and response.isError before accessing a page.
// A tool error may carry structuredContent.status === "artifact_limited";
// retain its artifact_id and limit, then stop this compilation attempt.
const page = response.structuredContent.artifact; // successful response only
```

Keep `page.content_id`, `page.basis_id` and qualifications. When another page is
needed, use `offset: page.next_offset` and `expected_content_id: page.content_id`
with the same artifact. A null next offset ends paging. Offsets count UTF-8 bytes,
not characters. Never call a partial page the whole artifact or fabricate content
identity when compilation is limited. Prose pages do not carry the artifact's
structured assertion collection (`structured_assertions_included: false`).
There is no flow-name filter: a relevant dossier may occur after unrelated pages,
and a page boundary may separate a diagram from its qualifications. Retrieve the
needed adjacent context within the task budget or report an incomplete result.
Do not manufacture a fact/edge identity from display names to obtain a citation.

An `artifact_limited` tool error reports trace or compiler work limits. Selected
compilation works from the frozen graph, not the discovery query. Smaller content
pages do not reduce that compilation work, so do not loop over smaller page sizes
or repeatedly reconnect hoping to bypass the refusal.

## Retained evidence

```js
const description = (await call("cartograph_evidence", {
  ...identity, kind: "retained_primary_source", mode: "describe",
  fact: candidate.reference, ordinal: 0, max_bytes: B
})).structuredContent.description;
const span = (await call("cartograph_evidence", {
  ...identity, kind: "retained_primary_source", mode: "read",
  target: description.target, max_bytes: B
})).structuredContent.span;
```

A fact observed in a captured Terraform state, Pulumi export or trace may also
have observed evidence: request it with `kind: "retained_observed_input"` in both
modes. Its target names the source's input identity (`input/src_…`), its `scope`
is `observed_input_only`, and it cites what was observed, never a declaration;
report it as observed, not as the declaring source. Read an observed target only
with that kind.

Copy the complete returned target unchanged, including nested source and captured
identities. Do not widen the span or construct a target from a filesystem path.
`description.next_ordinal` can select another range if relevant; do not enumerate
all ranges automatically. Preserve `scope`, `input_closure` and `redaction`. Read text
has passed the host's secret redaction: when `redaction.replacements` is above zero,
`[REDACTED]` marks a removed value, the text is not byte-identical to the captured
span, and the marker must never be reported as source content. If the advertised
file-validation or span limit prevents a read, a smaller response allowance does
not bypass that work limit. Evidence errors are not permission to run a provider.

Every example must check JSON-RPC errors and the returned CallToolResult's `isError` before reading
structured content. Do not treat an error payload as an empty successful result.

On hosts advertising typed evidence failures, `evidence_unavailable` means absent retained
evidence, not proof of forgetting or omission. `evidence_invalid` means retained
evidence failed validation (including underlying retention bounds); `stale_basis`
means the selected identity is no longer usable; `storage_unavailable` is operational.
Do not infer a more specific cause. When live capabilities explicitly advertise
`forgotten_receipt_classification.available`, `evidence_forgotten` reports a
durable forget classification for the exact current-basis receipt binding. It
returns no source span and is not permission to recapture, ingest or read the
checkout. This partial capability does not imply receipt-omission support;
`evidence_absence_reason_available` can remain false. Report generic unavailable
without inventing a cause when this specific result is absent.

When `receipt_omission_classification.available` is true, `evidence_omitted` with
structured `omission_kind` (`declarations`, `rules`, `store_capacity`) identifies
an authoritative current-fact producer omission at the checked basis. It returns
no source target or span and does not authorize recapture, ingestion or checkout
access. Unknown absence is never omission; `NotOmitted` does not mean a receipt
is available. This separate partial capability also leaves broad
`evidence_absence_reason_available` false. Complete and scoped modes advertise
this support independently; check the current session's live capabilities.

## Worked example: what handles `POST /owners/new`

Answer it from the recovered system alone, citing each step:

1. Find the endpoint: `cartograph_discover` with
   `{ query: { by: "text", text: "POST:/owners/new" }, labels: ["Endpoint"] }`.
   Use the candidate's exact `reference.id` (for example
   `ep:petclinic@POST:/owners/new`); never build an ID by hand.
2. Follow what handles it: `cartograph_query` with
   `{ scope: { type: "neighborhood", anchor: endpointId, hops: 1 }, kind: "edge", labels: ["HANDLES"] }`.
   For each edge whose `reference.source` is the endpoint, `reference.destination`
   is a handler, and the fact carries its
   tier, confidence and evidence references.
3. Follow what the handler does: the same query anchored on the handler with
   `labels: ["CALLS"]`. Neighborhoods are undirected, so the page also holds
   the handler's callers: keep only edges whose `reference.source` is the
   handler for what it calls, and report edges whose `reference.destination`
   is the handler as its callers. Use `hops: 2` for a wider view.
4. Quote source only through `cartograph_evidence` for a fact you cite.

Report the handler with the endpoint and edge references you read, and their
confidence tiers. An empty `HANDLES` page means this session shows no handler,
not that none exists. The same reads at the same basis return identical results,
so a reviewer can replay them.
