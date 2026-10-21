# ADR-0037 — Bounded persistent semantic vectors

- **Status:** Proposed
- **Date:** 2026-10-20
- **Deciders:** Cartograph maintainers (owner review pending)

## Context

The semantic preview embeds candidates, unresolved-hop queries and evaluation
pair texts on every invocation. Caching only candidates would leave repeated
provider calls even for an unchanged preview. Model tags can change, graph
stores can be cleared or recreated, and retained-source controls must account
for derived vectors at rest. Reusing vectors must not reuse stale evidence or
turn a prior evaluation outcome into authority for a new proposal.

## Decision

Cache vectors for every embedding text role, after deterministic redaction.
Key exact transmitted UTF-8 bytes by SHA-256 within a namespace that binds the
provider endpoint, observed model manifest digest, dimension and versioned
preprocessing/redaction recipe. Unknown or tag-only identities do not permit
persistent reuse. Observe the installed manifest around provider work; reject
changed identity rather than publish mismatched vectors. These observations
trust the local provider and are not cryptographic execution attestation.

Use a separately bounded SQLite vector store. Admit row counts, payload lengths
and aggregate sizes before decoding. Validate full embedding batches, finite
values and dimension consistency before atomic publication. Namespace metadata
pins dimension, recipe and manifest together. Deterministic sequence-based
eviction avoids wall-clock dependence. Concurrent writes retain the first
published value; every operation reads back that winning value before using it.
Counters distinguish hits, unique misses, calls, conflicts, invalid rows,
unpinned identities and evictions.

Bind app integration to the graph-store instance and generation, not an invented
system identifier. Graph clear, startup schema reset and retained-source forget
invalidate the generation and delete associated vector rows under a lifecycle
guard. A preview leases its generation before reading its graph and checks it
before publishing or returning. Invalidation must prevent an in-flight request
from restoring old vectors. Failure may cause extra cold misses, never reuse
under an invalid generation.

Rebuild ANN membership from the current graph on every preview. Recompute
scores, evaluation labels and thresholds, proposal gating and provenance; only
vectors are cached. T2 remains an ephemeral inferred overlay and cannot mutate
confirmed facts. Parent #259 remains open for structural dispatch filtering
and persistent ANN indexing; independent holdout acceptance belongs to #258.

## Consequences

An unchanged preview after reopen can issue zero embedding calls when its
required vectors remain resident and identity is pinned. Eviction or identity
changes can require calls; counters disclose this, without a latency claim.

Retained-source forget currently removes captures but leaves graph facts and
receipts. A future preview can re-embed those remaining facts. Cache deletion
therefore does not establish complete source-data forgetting, and logical
SQLite row deletion does not establish physical disk or WAL erasure.

Persistence and lifecycle changes require owner review. Verification uses
scripted providers and temporary stores, including reset races, corruption,
identity changes, concurrent publication and whole-preview cold/warm
equivalence. This decision does not authorize model downloads or live runs.

## Alternatives (≤3)

- **Candidate-only caching:** insufficient for zero embedding calls on an
  unchanged preview because queries and evaluation pairs would still embed.
- **Persist ANN files and proposal outcomes:** couples storage to index versions
  and risks stale membership or approval; rebuild these from current inputs.
- **Tag-only keys or generation-only forgetting:** mutable tags permit incorrect
  reuse, and a generation change alone leaves derived vectors at rest.
