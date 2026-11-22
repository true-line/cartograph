# ADR-0089 — In-app agents read official artifacts through `read_artifact`

- **Status:** Accepted (coordinator as the owner's delegate, 2026-11-22)
- **Date:** 2026-11-22
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #250, #255; ADR-0019, ADR-0044 (MCP reads), ADR-0058
  (citation IDs), ADR-0085 (native tool calling); SPEC-01

## Context

#250 asks for a first-class read surface for the agent runtime: graph
queries, spec artifacts, the grouped gap register and provenance lookups.
External agents now have all four over MCP: `cartograph_discover`,
`cartograph_query` (AC-1809, AC-1810), `cartograph_artifacts` and
`cartograph_read_artifact` (which include `gap-register-index`), and
`cartograph_evidence`.

The in-app investigation agents have only three acquisition actions:
`query_context`, `read_evidence` and `read_coverage_inventory`, plus
`replay_query` and the finish actions. They can follow facts and quote
source, but they cannot read the compiled artifacts that summarize the system
(data model, topology, flow dossiers, rule evidence, the gap register index).
So a domain analyst or gap triager rebuilds, from single facts, documents the
product has already compiled.

`read_coverage_inventory` (@6 and later) is the model for adding a read:

- It is a qualified action bound to the investigation's `context_id`.
- Its request is stored in the frozen input ledger as a manifest that holds a
  response hash and size, not the response.
- Its response counts against the input budget.
- It never becomes a citation.

## Decision

1. **One new qualified read action.** `read_artifact` has these fields:
   `{context_id, artifact_id, expected_content_id, offset, max_bytes}`.
   - It is decoded next to `query_context` and `read_coverage_inventory` in
     `QualifiedInvestigationAction`, with the same strict preflight,
     canonical-field check and context binding.
   - `artifact_id` must name an entry of the readable MCP catalogue
     (`mcp_reads::artifacts::readable_catalog`). There is one list for both
     surfaces.
   - `expected_content_id` is `null` on a first read and must be the issued
     content ID on a continuation.
   - `offset` is a UTF-8 byte offset into the artifact content.
   - `max_bytes` is at most `MAX_QUERY_BYTES`.
2. **Ledger manifests that leave old ledgers byte-identical.** The input
   ledger gains `artifact_reads: Vec<InvestigationArtifactReadManifest>`,
   which holds:
   - the request;
   - the artifact's `content_id`;
   - `response_hash` and `response_bytes`;
   - `next_offset`.

   The field is `#[serde(default, skip_serializing_if = "Vec::is_empty")]`, so
   every existing ledger, and its fingerprint, is unchanged. Artifact reads
   are only valid in coverage-qualified (v2) ledgers. Ledger validation,
   sealing, disclosure, continuation and repeat detection treat them like
   coverage reads. A repeat of an identical read is skipped and charged, not
   re-run.
3. **The host compiles from the frozen basis only.**
   - The host builds the artifact from the investigation's retained visible
     projection, using the same artifact compiler and byte-bounded
     `CompiledArtifact::page` as the MCP read session.
   - The basis is the projection the ledger's `scope_snapshot_id` names. For a
     whole-system investigation that is the complete visible graph. For an
     Area or Neighborhood investigation it is only the scoped projection, so
     the artifact describes the scope, and the page says so
     (`complete_system: false`); it is never labeled with the complete
     graph's `graph_snapshot_id`.
   - The host must reproduce `scope_snapshot_id` from the retained facts
     before compiling. A different basis is a stale refusal, never a page from
     another revision or a wider graph.
   - A continuation whose `expected_content_id` differs from the compiled
     content is stale.
   - Trace and compile limits are the MCP read limits. A limited compilation
     is refused with its limit, never returned as a truncated document.
4. **Artifact text is context, not evidence.**
   - An artifact page cannot be cited.
   - A claim still needs exact fact citations, which the agent reaches with
     `query_context` or `read_evidence` from the IDs the artifact names.
   - The page is supplied to the model as untrusted data, under the existing
     data framing.
5. **Budget.**
   - Each read is one acquisition.
   - Its response bytes count against the same input byte budget as queries
     and coverage reads.
   - Refusals for the action, budget and capacity use the existing typed
     reasons.
6. **Only a new specialist version offers it.**
   - A new capability, `ArtifactReads`, is granted only to a new, Offered
     `domain-analyst@30`, built on @29 with the native tool runtime. Its
     native tool is `read_artifact`.
   - `domain-analyst@29`, which the D1-D3 measurement is frozen on, and every
     other version and their byte goldens stay unchanged.
   - Other roles (gap triager, spec writer) can adopt the capability later in
     their own new versions.
7. **The UI names the read.** Investigation progress shows "read artifact
   *title*" with the page range. The saved task shows each artifact read's
   manifest, like a coverage read.

## Consequences

- In-app agents can start from the documents Cartograph already compiled,
  including the grouped gap register index, and then cite the underlying
  facts. This is the substrate #250 asks for, with the same artifact
  catalogue external agents use.
- The change is additive in every persisted form. Older tasks, ledgers,
  fingerprints and specialist goldens keep their bytes.
- Compiling an artifact from a frozen basis costs a full projection compile
  per read. Hosts may cache one compilation per `(scope_snapshot_id,
  artifact_id)` for the life of an investigation, but the cache never crosses
  a basis.
- Artifact pages are summaries. An agent that answers from them alone, with
  no fact citations, still fails the existing citation obligations.

## Alternatives

1. **Let in-app agents call the local MCP server.** Rejected: MCP sessions
   are an external trust boundary with their own grants and limits. Their
   pages would sit outside the frozen input ledger, the acquisition budget
   and the sealed disclosure, so a run could not be replayed or audited.
2. **Supply selected artifacts in the first prompt.** Rejected: the host
   would have to guess which artifacts a task needs, every page would spend
   prompt capacity whether used or not, and every specialist's golden would
   change.
3. **Add an artifact scope to `query_context`.** Rejected: query pages are
   fact pages whose facts become citations. Artifact text is a summary, not
   evidence, and mixing the two would weaken the query manifest and citation
   contracts.

## Implementation order

1. Agent core: the action, decoding, ledger manifest, validation and repeat
   detection; budget and refusal accounting.
2. Host: dispatch over the artifact compiler at the frozen basis, journaling,
   continuation and storage validation.
3. `domain-analyst@30`: the capability, prompt, visible action schema and
   native tool definition.
4. UI progress and saved-task rendering.
