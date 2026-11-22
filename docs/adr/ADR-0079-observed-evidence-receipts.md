# ADR-0079 — Observed evidence gets its own receipts and bindings

- **Status:** Accepted (coordinator as the owner's delegate, 2026-11-21)
- **Date:** 2026-11-21
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #385 (manifest state and trace evidence); ADR-0080 (input captures, AC-1041 and AC-1043); ADR-0024; ADR-0046 (retention); SPEC-06

## Context

Since ADR-0080, observed evidence from Terraform state, Pulumi exports and
OTLP traces cites a retained input capture, `input/<source id>`. That evidence
is still unverified, because:

- A fact has exactly one source binding. The binding tables
  `source_node_bindings` and `source_edge_bindings` are keyed by the fact and
  hold one `receipt_id`.
- The binding's receipt must belong to the registered source itself.
  `bound_receipt` and the scoped MCP evidence reader both require
  `receipt.source_id == source.source_id`.
- The facts that observation enriches usually already have their own binding.
  Endpoints have a TS or Python receipt, Pulumi resources have a TS receipt,
  and channels may too. Terraform resources have none, because HCL is not a
  captured lane.

Replacing the primary binding would lose the declaration citation. It would
also make "verified" mean two different things.

## Decision

1. **An observed receipt kind.** `obs-input-v1` receipts are provenance-only.
   One is minted for each fact whose `observed_prov` (or, for a fact the
   observation created, whose `prov`) cites a captured input, and only when
   every evidence entry cites that one captured file.
   - Its ranges are the exact byte spans the observation evidence records in
     the input capture: the resource address in Terraform state or a Pulumi
     export, the span record in a trace.
   - The receipt's `source_id` and `repo_key` are both the input identity
     `input/<source id>`, and its file is the input capture's file. A receipt
     of a registered source can never claim an `input/` repo, and an input
     receipt can never claim another repo.
2. **Stored and retained with the owning source (AC-1045).** Observed receipts
   live in the same receipt store, owned by the input identity. They are
   admitted within the owning registered source's share (ADR-0046), less what
   that source's own receipts took in the same recovery; a receipt that does
   not fit is left out. The retained-source preview counts them and binds
   their ids in its fingerprint, and forgetting the source reclaims them, with
   the same pins from staged proposals, investigations and work packets.
   Superseded-receipt reclaim follows their bindings (decision 3).
3. **A second, observed binding role.** New tables `source_node_observations`
   and `source_edge_observations` have the same shape as the binding tables.
   Each holds the fact, the repo key, the receipt id and the complete fact
   digest. They are published in the same transaction as the primary
   bindings.
   - A fact can have a primary binding, an observed binding, both or neither.
   - The primary binding's meaning does not change.
4. **Readers accept the input identity of the same source.** A receipt is
   admissible for a primary binding only when its `source_id` is the
   registered source's id. For an observed binding, only the source's
   `input/` identity is admissible.
   - The app describes and reads the observed binding through its own
     `describe_observed_source` and `read_observed_source`, beside the
     unchanged primary `describe_captured_source` (AC-1049).
   - Scoped MCP evidence names the role by its existing `kind`:
     `retained_primary_source` or `retained_observed_input`. An observed
     target names the input identity, so the target carries its role and must
     match the kind (AC-1051).
5. **What "verified" means.** A fact is observed-verified when its observed
   binding resolves and its ranges read back. Curated-context activation keeps
   requiring a primary binding unless it says otherwise. Observed verification
   is shown and cited, but it never stands in for a declaration.
6. **Uncaptured inputs stay unverified.** An input outside the manifest folder
   or past the capture limits mints nothing. Coverage names it, as ADR-0080
   already does.

## Slices

1. **AC-1045:** receipt minting for state, Pulumi and traces, stored within
   the owner's share, plus retention accounting and forget.
2. **AC-1047:** the observed binding tables and their publication, plus
   superseded-reclaim keep-sets that include observed bindings.
3. **AC-1049:** the app read paths (describe/read) and the UI "observed in"
   citation.
4. **AC-1051:** the scoped MCP evidence role.

## Consequences

- Observed facts become citable down to exact bytes without disturbing
  declaration citations.
- The receipt store accepts a second owner form, `input/<source id>`, in its
  integrity checks.
- There is one more binding table pair in the graph schema, which bumps the
  binding meta version and adds a migration (AC-1047).
- The receipt store sees more receipts per source. They stay inside its
  existing share.
