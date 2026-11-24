# ADR-0072 — Producer input closure, reconciliation results and external evidence captures

- **Status:** Accepted (2026-11-22, by the project coordinator as the owner's delegate)
- **Date:** 2026-11-18
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #385 (parent); [SPEC-04](../SPEC-04_source-capture.md) / ADR-0022; [SPEC-05](../SPEC-05_registered-sources.md) / ADR-0023; [SPEC-06](../SPEC-06_primary-source-receipts.md) / ADR-0024; [SPEC-08](../SPEC-08_local-rule-definitions.md) / ADR-0026; [SPEC-09](../SPEC-09_captured-task-basis.md) / ADR-0027; [SPEC-12](../SPEC-12_curated-projection.md) / ADR-0061; SPEC-03 / ADR-0021; R-INT-1, R-INT-2, R-INT-3

> **Accepted 2026-11-22.** This ADR changes provenance and tier-adjacent
> semantics: it adds a third value to what a receipt says about its inputs,
> a derived reconciliation state for staged proposals, a new registered source
> kind and capture kind, and (slice 7) new provenance for T1 enrichment facts,
> which changes their fact bytes and the graph hash. It changes no tier and no
> confidence. Each slice below is its own PR. Answers to the questions under
> Consequences: (1) `RecaptureCorroborated` items make a record eligible,
> counted apart from `RetainedMatch`; (2) eligibility does not require an
> established closure until slice 6 has landed; (3) moot: external evidence
> identity and its receipts were built under ADR-0080 and ADR-0079
> (AC-1041..1051), which replace slices 7 and 8 here.

## Context

The capture prerequisites of #385 are on main. SPEC-04 retains selected bytes
under a `CaptureFileRef`. SPEC-05 gives every source a host-owned `source_id`
and `repo_key`. SPEC-06 has the TypeScript producer parse a `CapturedFile`
and mint a per-fact `Receipt` (contracts `ts-primary-v1:` to `ts-primary-v3:`,
receipt store schema 4). SPEC-08 adds local-definition ranges. SPEC-09 has
agent tasks read receipt ranges and stage `StageContentV2` with a
`TaskSourceBasisV2`. SPEC-12 projects accepted proposals.

Four things in #385 are still open.

1. **Input closure.** Every `Receipt` content carries
   `source_scope: primary_source_only` and
   `input_closure: input_closure_not_established`. `TaskEvidenceKind::
   CapturedPrimarySource` copies `InputClosureStatus::
   InputClosureNotEstablished`. Each enum has one value. Nothing says which
   inputs a fact depends on beyond the one captured file.
2. **Reconciliation.** `StagedProposal.evidence_binding` is
   `working_tree_unverified` for every v1 record. `context_status` is
   `awaiting_reconciliation` for every record, v1 and v2. Neither can change:
   both are inside immutable staged content. No operation produces the
   "explicit reconciliation result against the complete original task basis"
   that #385 acceptance criterion 7 asks for. SPEC-09's `BasisAssessment`
   compares a v2 record with the current graph. It returns
   `legacy_unverified` for v1 and never establishes anything new.
3. **External evidence.** `ManifestRepo.state_json`, `pulumi_json` and
   `otel_jsonl` name files relative to the manifest. Those files are not in
   any registered source. They are not captured, and T1 enrichment from them
   cites no capture. #385 says they need their own source and capture
   identities, not a borrowed repository commit.
4. **Verification.** No test shows that checkout bytes and Git blob bytes
   cannot be swapped for one another through a receipt. None shows that two
   linked worktrees and a manifest-local entry get distinct capture
   identities through the production path.

## Decision

### 1. Input closure is a typed, content-addressed input set

The **input closure** of an emitted fact is the complete set of inputs whose
change could change that fact's bytes, or whether it is emitted at all, under
one producer contract. A closure is **established** only when all three of
these hold:

- the producer contract declares the input classes it reads for that fact
  kind. The list is closed and versioned with the contract;
- every input actually consulted is bound below. A looked-up path that was
  not found counts as consulted;
- the final emitted fact digest is validated after every later enrichment, as
  SPEC-06 already requires for receipts.

Anything less is `not_established`, with fixed reason codes. An established
closure is not business truth. It claims neither runtime values nor current
freshness, and it never changes tier or confidence. It states one thing: this
fact is a function of exactly these bound inputs.

A new immutable metadata-only record holds the closure:

```rust
struct InputClosureV1 {
    schema_version: u32,              // exactly 1
    closure_id: String,               // "closure-v1:" + BLAKE3; excluded from its own preimage
    producer_contract: String,        // e.g. "t0.adapter-ts/direct-lexical-v4"
    producer_packages: Vec<String>,   // exact pins, as receipts record them
    parameters: ClosureParameters,    // typed host inputs; no roots, no timestamps
    inputs: Vec<ClosureInput>,        // sorted, unique, bounded
}

enum ClosureInput {
    /// Exact retained bytes the producer read (SPEC-04 CaptureFileRef).
    CapturedFile { file: CaptureFileRef },
    /// Membership of an enumerated selection: the capture manifest digest
    /// plus the walk policy that produced it, including skipped symlinks.
    Enumeration { source_id: SourceId, capture_id: String, walk_policy: String },
    /// A path the producer looked up and did not find in that enumeration.
    /// Absence is relative to the enumeration, never "absent on disk".
    AbsentFromEnumeration { source_id: SourceId, capture_id: String, path: String },
    /// A value read from a captured system manifest (decision 3), addressed
    /// by capture file and a fixed field path.
    Declared { file: CaptureFileRef, field: String },
    /// An external evidence capture (decision 3).
    External { file: CaptureFileRef, media: ExternalMedia },
    /// A fact this producer consumed, with its own closure.
    UpstreamFact { fact: FactKey, fact_digest: String, closure: ClosureRef },
}

enum ClosureRef { Established { closure_id: String }, NotEstablished }
```

**Canonical encoding.** Both identifiers in this ADR, `closure_id` and
`result_id` (decision 2), use the encoding SPEC-06 already uses for receipt
IDs. The preimage is a fixed versioned domain prefix
(`cartograph:input-closure:v1\0` or `cartograph:reconciliation-result:v1\0`),
followed by compact UTF-8 JSON of the complete record. Object keys are sorted
recursively by byte order; arrays keep their specified order; enum variants
use their `snake_case` tag; integers are plain decimal; there is no
whitespace and no optional-field omission. **The id field is excluded from its
own preimage:** the record is serialized without `closure_id` (or
`result_id`), hashed with BLAKE3, and the id is the prefix (`closure-v1:` or
`reconcile-v1:`) plus the lowercase hex digest. A decoder recomputes the id
and rejects a record whose stored id does not match.

`ClosureParameters` holds the registered `repo_key`, the source-walk policy
version, the layer hints in effect and the grammar selection rule version.
These are inputs because changing any of them can change the fact. A closure
with any `UpstreamFact { closure: NotEstablished }` is not established, with
reason `upstream_closure_incomplete`. Closures form a DAG through
`UpstreamFact`. A fact is never its own upstream.

The receipt says which case applies. Receipt contract **v4**
(`ts-primary-v4:`, compact like v3) replaces the single-valued `InputClosure`
with:

```rust
enum InputClosure {
    InputClosureNotEstablished { reasons: Vec<ClosureGapReason> }, // sorted, nonempty
    Established { closure_id: String },
}
```

Receipt contracts v1 to v3 keep their exact bytes, IDs and meaning: no closure
is attached to them after the fact. `source_scope` stays `primary_source_only`
in v4. Closure records are persisted in the receipt store before graph
publication, in the same transaction as their receipts (store schema 5).
Retention is unchanged: forgetting a source removes its bytes, the closure
record stays as metadata, and its inputs read as unavailable.

The fixed `ClosureGapReason` codes are `producer_not_participating`,
`cross_file_input_unbound`, `external_input_unbound`, `remote_reference`,
`upstream_closure_incomplete`, `enrichment_changed_fact`,
`cache_reuse_unproven` and `limit_exceeded`.

**Per producer**, closure means the following. A producer or fact kind not
listed is `producer_not_participating`.

| Producer (crate) | Fact kinds | Inputs that must be bound | Closure state after this ADR |
|---|---|---|---|
| TS direct lexical (`adapters-lang-ts`, receipts v1–v3) | rule exits, owners, GOVERNS, declarations, local definitions (SPEC-06/08) | `CapturedFile` of the parsed file; contract and package pins; `repo_key`; grammar selection by path | Established (slice 5) only when a test shows that no other file in the walk changes the fact, and the post-enrichment digest matches. Otherwise `enrichment_changed_fact` |
| TS directory resolution (`adapters-lang-ts` directory completion) | directory-proven CALLS and their Gaps; Pulumi relative-import edges; client routes and navigation; Next API routes; service-worker registrations; GraphQL target tests | `Enumeration` of the walk; `CapturedFile` of each file whose extraction contributed; `CapturedFile` or `AbsentFromEnumeration` for each probed `package.json`, `tsconfig.json` (`adapters-fw/tsconfig.rs`) and `.gitignore`; walk policy version | Established in slice 6. These facts carry no receipt today, so slice 6 adds one |
| Framework config and events (`adapters-fw`, `events`) | config-read facts (ADR-0059), channel stitching, HTTP bindings | upstream TS facts; every env/config file the resolver consulted, plus the probes it missed; manifest `env`, `services` and `gateway_routes` as `Declared`; resolver precedence version | Slice 9. Multi-source: inputs may span several `source_id`s |
| IaC HCL (`iac`) | T0 resources, GRANTS, policy statements | `.tf` files of the module walk; locally resolved module sources; probed `*.tfvars` | Slice 9. A registry or remote module source is `remote_reference`: never fetched, never established |
| T1 state, Pulumi and trace enrichment (`iac`, `dynamic`) | observed properties and edges; trace-filled channel slots | the upstream T0 fact and its closure; `External` capture; the `Declared` manifest field that named the file | Slice 8, after external captures exist (decision 3) |
| Declarative file readers (`ingest`: pipelines, environments, schedules, toolchain, api_docs) | pipeline (ADR-0060), environment and workload (ADR-0070), schedule and toolchain facts | each read file; probes for each conventional location | Slice 9, one reader per PR |
| ADR recovery and relinking (ADR-0012) | ADR nodes and links | captured markdown across every planned root; `UpstreamFact` for every linked target | Slice 9 |
| Go, Java, Kotlin, Python and Rust adapters; WASM plugins | all | none bound | Stays `producer_not_participating`. A plugin closure later also needs the WASM artifact digest as an input |
| T2 semantic, T3 agentic | — | — | No input closure. T3 proposals use their task basis and decision 2 |

The TS parse cache stays bypassed in every participating lane. A cache entry
may be reused for an established closure only when it is keyed by that exact
`closure_id`. Otherwise the code is `cache_reuse_unproven`.

New stages carry the closure state. `StageContentV3` and `TaskSourceBasisV3`
widen `InputClosureStatus` the same way receipts do: `Established
{ closure_id }` is copied from the receipt actually read, never computed at
staging. V1 and v2 content, decoders and hash domains stay frozen.

### 2. Reconciliation is a separate, append-only result

Reconciliation never edits a staged record. `evidence_binding`,
`context_status`, `proposal_id` and the review revision stay exactly as
stored. A v1 record is `working_tree_unverified` about its *original* bytes
for good. #385 says that matching a later blob cannot change that.

A host operation `reconcile_staged(proposal_id)` runs after an explicit
recapture and recovery of the record's sources. It is never implicit and never
run on a timer. It writes one immutable `ReconciliationResultV1` to a new
append-only table beside the staged rows in `proposals.sqlite`. The table
holds metadata only, no source text.

```rust
struct ReconciliationResultV1 {
    schema_version: u32,               // exactly 1
    result_id: String,                 // "reconcile-v1:" + BLAKE3; excluded from its own preimage
    proposal_id: String,               // binds the complete original content
    proposal_schema_version: u32,      // 1, 2 or 3
    recovery: RecoveryBasis,           // job id, graph_snapshot_id, sorted (source_id, capture_id)
    method: ReconcileMethod,           // legacy_text_v1 | captured_span_v2
    slot: SlotReconciliation,          // source, Gap, relation: present / missing / gap_resolved
    evidence: Vec<ItemReconciliation>, // exactly one per original EvidenceBasis, original order
    candidates: Vec<CandidateReconciliation>, // exactly one per original CandidateBasis
    outcome: ReconciliationOutcome,
}

struct ItemReconciliation {
    evidence_id: String,
    original: OriginalBinding,        // working_tree_unverified | captured { receipt_id, captured }
    outcome: ItemOutcome,
    /// The receipt range at the recapture that the comparison used;
    /// present only for retained_match and recapture_corroborated.
    reconciled: Option<ReconciledOrigin>, // receipt_id, inventory index, CaptureSpanRef, input_closure
}

enum ItemOutcome {
    RetainedMatch,                         // v2/v3 captured item
    RecaptureCorroborated,                 // v1 or v2 working-tree item
    Changed { reason: ChangedReason },     // span_bytes_differ | fact_digest_differs
    Missing { reason: MissingReason },     // fact_absent | path_not_captured | span_out_of_bounds
    Unverifiable { reason: UnverifiableReason },
        // no_participating_receipt | lossy_text | summary_procedure_unknown | validation_byte_limit
}

enum ReconciliationOutcome { Reconciled, Changed, Missing, Unverifiable }
```

The comparisons are exact. No fuzzy or relocated match is ever made.

- **Captured item** (v2/v3 `CapturedPrimarySource`). Recovery must produce a
  current receipt for the same `FactKey` with the same role and index. Its
  fact digest must equal the staged `TaskFactSelection` digest. BLAKE3 of its
  raw span bytes must equal the staged `text_hash`: strict UTF-8 means that
  text hash is a hash of the raw span. The span length must equal
  `text_bytes`. If all hold, the outcome is `RetainedMatch`. The original
  capture need not still be retained.
- **Working-tree item** (every v1 item, and v2 `WorkingTreeUnverified`).
  Recovery must produce a participating receipt for the cited fact whose
  range covers the staged `EvidenceRef` path and offsets exactly. The
  recaptured span must decode as **strict** UTF-8, and BLAKE3 of those bytes
  must equal `text_hash` under the frozen v1 construction (`read_span_exact`:
  slice, then `from_utf8_lossy`). If all hold, the outcome is
  `RecaptureCorroborated`. If the span is not valid UTF-8, the outcome is
  `Unverifiable { lossy_text }`, because many byte strings decode to the same
  lossy text. `EvidenceRef.commit_sha` is never consulted.
- **Candidates.** The summary is rebuilt from the re-recovered fact with the
  frozen candidate-summary procedure, and compared by hash and length. Slice 1
  pins that procedure as `candidate-summary-v1` and proves from history that
  it has not changed since #384. If it has, records older than the change
  are `Unverifiable { summary_procedure_unknown }`.
- **Outcome precedence:** `Changed` > `Missing` > `Unverifiable` >
  `Reconciled`. Every item outcome is always reported.

**State.** A record's reconciliation state is derived, never stored on the
record. It is the outcome of its latest result while that result's
`RecoveryBasis` is still current, and `reconciliation_stale` once it is not.
This applies to every outcome, not only `reconciled`: a `changed`, `missing`
or `unverifiable` result is also a claim about one recovery, and it goes
stale when that recovery stops being current. The SPEC-09 comparisons are run
against the *result's* `RecoveryBasis`, not the original snapshot.

```
awaiting_reconciliation --reconcile--> reconciled | changed | missing | unverifiable
reconciled | changed | missing | unverifiable
    --RecoveryBasis no longer current (graph, association or retained-byte change)-->
    reconciliation_stale
any state    --reconcile again--> the new result's outcome
```

Review decisions, acceptance and restart do not move the state. An
operational failure (busy, storage error or limit) writes no result and
leaves the state where it was. Results are never deleted by job cleanup.
`StagedProposal` gains an optional derived `reconciliation` DTO field. It is
outside content identity, like the review fields.

**Effect on SPEC-12.** In slice 4 the projection reads a record's evidence
from its current `reconciled` result instead of from its staged origin. Its
`unverified` reason then no longer applies. Every other state check is
unchanged. `RetainedMatch` and `RecaptureCorroborated` are counted separately
in DR-4 `source_support`, so a corroborated legacy item is never shown the
same way as originally captured evidence (R-INT-2). `changed`, `missing`,
`unverifiable` and `reconciliation_stale` keep the record listed with that
reason. The policy version becomes `curated-policy-v3`, which changes the
curated revision.

### 3. External evidence gets its own source and capture identity

- **Registration.** SPEC-05 gains a third registration kind,
  `external_evidence`, keyed by the canonical file path. It is not a
  repository, and it gets its own random `source_id` and repo key
  `external/<source_id>`. A file under a registered repository root is still
  a separate external registration when a manifest declares it as external
  evidence. It never borrows that repository's commit, and its `repo_key`
  never matches the repository's. The system manifest file itself is
  registered the same way, with media `system_manifest`.
- **Capture kind.** SPEC-04 capture manifest **v2** adds
  `CaptureKind::ExternalEvidence { media: ExternalMedia }`, with
  `ExternalMedia` = `terraform_show_json`, `pulumi_json`, `otlp_json_lines`
  or `system_manifest`. The media is part of the capture ID, so one byte
  sequence declared as state and as a trace has two capture IDs. V1 manifests
  and their IDs are unchanged, and store schema 3 accepts both.
- **What identifies it.** Bytes, kind and source only. A Terraform `serial`
  or `lineage` and trace timestamps are content. They may be reported, but
  they are never capture identity or a freshness claim.
- **Limits and no capture.** SPEC-04's limits apply unchanged: 16 MiB per
  file, 128 MiB per capture. **No capture means no enrichment.** When an
  external file is over a limit, missing, unreadable or otherwise not
  captured, the producer does not run T1 enrichment from it. The ingest
  result records a typed outcome instead, `ExternalEvidenceOutcome::
  NotCaptured { declared_by, media, reason }`, where `declared_by` names the
  manifest field and `reason` is one of `limit_exceeded`, `missing`,
  `unreadable` or `unsupported_path`. This is shown in coverage like any other
  skipped input. No T1 fact or property is emitted from uncaptured bytes, and
  no provenance shape without a `CaptureFileRef` is introduced. The T0 facts
  it would have enriched stay as they are.
- **Provenance.** In slice 7, T1 facts and properties from external evidence
  cite the external repo key and a `CaptureFileRef`, with `commit_sha` unset.
  Every such fact has one, because of the rule above.
  This changes those facts' bytes, so the graph fact schema is bumped and the
  existing rebuild contract applies. The T0 fact being enriched is unchanged
  (R-INT-1).

### 4. Verification that crosses producers

Each slice's tests go in that slice. Slice 2 adds three fixtures that every
later producer slice must extend.

- **EOL and checkout filter.** A Git fixture uses `.gitattributes` with `text
  eol=crlf` and a configured clean/smudge filter. Its checkout bytes differ
  from its blob bytes. Working-tree capture and Git-object capture of the same
  paths get different digests and capture IDs. Every participating receipt
  and closure names the capture it parsed. The capture path never runs the
  filter, and `commit_sha` on a working-tree receipt never makes it a Git
  capture. Reconciling a record captured from the checkout against a
  Git-object recapture gives `changed`, not `reconciled`.
- **Linked worktree.** A main worktree and a linked worktree of the same
  repository at the same commit, with one file different, get distinct
  `source_id`s and capture IDs. Unchanged files have equal per-file digests.
  Neither worktree's receipts, closures or reconciliation results are
  accepted for the other.
- **Manifest-local.** A manifest with a local `file://` entry, a `state_json`
  and an `otel_jsonl` captures the repository as a local source and each
  evidence file as `external_evidence`. Each gets a distinct identity. None
  borrows the repository commit.

### Implementation slices, in order

Each slice's PR uses `Refs #385`, so #385 stays open until the last one.
Each PR reserves its own AC/T ids at the time and carries its SPEC and
traceability rows. Slices 7 and 8 are replaced by ADR-0080 and ADR-0079.

1. **Pure reconciliation types** (`agents`). `ReconciliationResultV1`,
   canonical identity, a strict decoder, outcome derivation from supplied
   comparisons, and the pinned `legacy_text_v1` and `candidate-summary-v1`
   procedures. *Tests:* literal v1 and v2 rows; one test per item outcome;
   precedence; tampered or unknown fields rejected; result ID stable under
   reordering only where the order is not meaningful.
   *As built (AC-1066):* `agents::reconciliation`. The host supplies one
   `RecapturedItem` per staged item and one `RecapturedCandidate` per
   candidate; the module derives every outcome. The slot is reported as
   source and relation (`present`/`missing`) and Gap
   (`present`/`missing`/`gap_resolved`); a missing endpoint makes the record
   `missing`, and a Gap that deterministic recovery now resolves makes it
   `changed`. `candidate-summary-v1` is `"{label}: {name}"` (`name`, else
   `path`, else the node id); it has been the summary since staging began
   (#384, `cb0d4a69`), so no record is `summary_procedure_unknown`. Task
   assembly now calls it, so a change has to be a new version.
2. **Cross-producer fixtures** (tests only, plus fixes they expose). The
   three fixtures of decision 4, run against today's TS producer.
   *Tests:* as listed there.
   *As built (AC-1068):* the EOL/filter and linked-worktree fixtures are in
   `src-tauri/src/primary_source_tests.rs` and pass against today's TS
   producer with no fix needed. The filter fixture checks the filter never
   runs (its marker file is never written) and that `compare_captured` on the
   checkout span against the Git-object recapture gives `changed` with
   `span_bytes_differ`. The manifest-local fixture
   (`a_managed_source_and_its_manifest_inputs_never_share_an_identity`,
   `src-tauri/src/main.rs`) follows ADR-0080, which replaced the separate
   `external_evidence` registration: a managed `file://` repository's
   `state_json` and `otel_jsonl` share one input capture under
   `input/<source id>`, apart from the repository's captures, each file with
   its own path and digest, and the observed evidence cites that identity,
   never the repository key or commit.
3. **Host reconciliation** (`src-tauri` proposals and task_evidence, UI
   history). `reconcile_staged`, the append-only table, the derived state and
   the DTO field. *Tests:* edit, delete or move the cited span between
   staging and recapture (gives `changed` or `missing`); an untouched file
   gives `reconciled`; invalid UTF-8 gives `unverifiable`; a later graph
   change gives `reconciliation_stale`; restart keeps results; acceptance
   changes nothing; staged rows are byte-identical before and after; no
   source text is stored.
   *As built so far (AC-1070):* the store half. `ProposalStore` opens a
   `staged_reconciliation_results` table (`result_id` unique, `proposal_id`,
   canonical `result_json` up to 64 KiB, `recorded_at`) with triggers that
   refuse every update and delete. `record_reconciliation` validates the
   result against the staged record in the same transaction and is
   idempotent; `reconciliations` reads up to 64 newest first, each strictly
   decoded and bound to its record.
   *Host (AC-1072):* `task_evidence::reconcile`. `reconcile_staged` runs as
   its own durable job (`reconcile-staged:<proposal id>`), whose id is the
   result's `RecoveryBasis.job_id`; the recovery names every capture whose
   receipt was read (none when every cited fact is gone). A v1 item names no
   fact, so it is looked up under the slot's source and Gap facts (source
   items) and the candidates that list it; the most specific outcome wins.
   The Gap reads `gap_resolved` when its node is no longer a Gap, or is gone
   and the source now has the relation to a real node. The derived state is
   `reconciliation_stale` once the graph snapshot moved, a reconciled span's
   capture is no longer retained, or its fact's current association names
   another receipt. `staged_reconciliation_history` returns the state and up
   to 64 results.
   *As built (AC-1074):* the review dialog reads the derived state and the
   results on request and offers "Reconcile now". The state is derived per
   read, never a `StagedProposal` field, so listing proposals stays a
   store read.
4. **SPEC-12 consumption** (`context-hub`). `curated-policy-v3`,
   reconciled evidence, separate DR-4 counts. *Tests:* each reconciliation
   state maps to its listed reason; a reconciled v1 record leaves
   `unverified`; corroborated and retained items are counted apart; the
   curated revision changes with policy v3; export and MCP agree.
   *As built (AC-1076):* the policy is `curated-policy-v5`, since v3 and v4
   were taken by findings and attested records. The host supplies a record's
   reconciliation in place of the SPEC-09 assessment when it is current and
   `reconciled`, `changed` or `missing`; a stale or unverifiable one decides
   only a record the assessment cannot make eligible, so reconciling never
   leaves a captured record worse off than not reconciling. A reconciled
   record skips the SPEC-09 `stale` check, since the result already compared
   it with the recovery the read is at. The non-matching outcomes are
   `unverified` reasons, which keeps the state order.
5. **Closure record and TS single-file closure** (`adapters-lang-ts`,
   receipt store schema 5, staging v3). `InputClosureV1`, receipt v4 and
   `TaskSourceBasisV3`. *Tests:* mutating every other walk file leaves the
   direct-lexical digest and closure ID unchanged; a mutation that enrichment
   does see gives `enrichment_changed_fact`; v1 to v3 receipts are
   byte-identical after migration; closure and receipt persist atomically.
   *As built (AC-1078, AC-1080):* `InputClosureV1` lives in
   `adapters_lang_ts::captured::closure`; file bindings, fact keys and fact
   digests are validated as receipts validate them. On the wire a v4
   receipt's `input_closure` is internally tagged by `state`
   (`established` with `closure_id`, or `input_closure_not_established`
   with `reasons`); v1 to v3 keep the bare legacy string. A producer mints
   v4 from a freshly minted v3 TypeScript receipt with
   `Receipt::with_closure`; lane receipts stay v3.
   *As built (AC-1082):* receipt store schema 5 adds `input_closures`
   (closure id, owner, canonical bytes) and `receipts.closure_id`. A closure
   is stored with the first retained receipt that names it, charged like a
   shared header, and deleted with the last; v4 rows use the compact header
   form, whose `input_closure` is the legacy string for v3 and the closure
   state for v4.
   *As built (AC-1084):* the captured directory lane mints v4. A rule or
   declaration of a freshly parsed file is established on that file's
   closure (walk policy `source-walk-v1`, grammar rule
   `grammar-by-path-v1`, no layer hints). A cached parse records the
   closure it was made under and reuses it only for that exact
   `closure_id`, so an unchanged capture's receipts equal a cold walk's;
   any other reuse is `cache_reuse_unproven`. A completion-decided File, TestCase or Gap
   receipt is `cross_file_input_unbound`. A fact enrichment changes keeps
   no receipt at all, as before, so `enrichment_changed_fact` is not
   minted. Every `CaptureFileRef` carries its capture id, so a closure id
   changes with any recapture; the test therefore holds the fact, its spans
   and the closure's inputs fixed apart from that identity. The
   single-file lane still mints v3. Each established compact receipt
   carries its file's record (one shared allocation per file, charged once
   against the extraction receipt budget), so the store retains it with the
   receipt without a separate hand-off.
   *As built (AC-1086):* `TaskSourceBasisV3` is the `TaskSourceBasisV2`
   struct at `schema_version` 3, and `StageContentV3` the v2 envelope over
   it, so no reader forks. A captured origin's `input_closure` is a
   `TaskInputClosure`: the bare legacy string under schema 2 and for v1 to
   v3 receipts, else the v4 receipt's own state in its wire form. Schema 3
   fingerprints under `task-source-basis:v3`, hashes the prepared task under
   `prepared-agent-task:v3` and stages under `proposal-stage:v3`. Nothing
   prepares a schema-3 basis yet; the host copies receipt states in the next
   slice.
   *As built (AC-1088):* host task preparation now always records schema 3
   and copies `Receipt::closure()` onto each captured origin, so new stages
   are v3; the curated projection reads v3 like v2. A reconciliation
   result's `ReconciledOrigin.input_closure` still records the legacy state.
6. **TS walk closure** (directory completion). Enumeration and probe binding,
   and receipts for directory-proven facts. *Tests:* adding the target of a
   previously unresolved import changes the closure and the fact; adding an
   unrelated file changes `Enumeration` and the closure ID but not the fact;
   `tsconfig` and `package.json` probes recorded as present or absent; a
   skipped symlink is named in the enumeration.
   *As built (AC-1090):* completion still reads its resolution configs
   (tsconfig and jsconfig, Vite and webpack configs, `package.json`) from
   the live tree. The selection now lists them, the host retains them with
   the TS/JS files, and a captured walk reports them bound
   (`IncrementalStats::resolution_configs`) only when the capture's config
   set is exactly the set read, byte for byte. A walk closure may name them
   only then; otherwise completion-decided facts stay
   `cross_file_input_unbound`.
   *As built (AC-1092):* completion's other live lookups are recorded: Next
   router roots (directory exists), imported non-source files (file
   exists), the root `vercel.json` read and whether a dependency type
   allowlist exists. A captured walk reports them bound
   (`IncrementalStats::completion_probes`) only when the capture answers
   each alike: a directory found holds a captured file, a path not found is
   absent from the capture (listed for `AbsentFromEnumeration`), a root file
   read is retained with those bytes. An existing imported asset is not
   retained and an allowlist may lead completion into installed packages,
   so either leaves them unbound. The selection lists a root `vercel.json`.
   *As built (AC-1094):* when both are bound, a completion-decided File,
   TestCase or Gap receipt of a proven parse is established on its file's
   walk closure (walk policy `source-walk-v2`): the `Enumeration` of the
   capture, which binds every retained file and so every other file of the
   walk, the file's `CapturedFile`, each bound config and root file read,
   and an `AbsentFromEnumeration` per path looked up and not found. One
   record per file is shared by its decided receipts. Otherwise the reason
   stays `cross_file_input_unbound`, or `cache_reuse_unproven` for a parse
   reused under another closure, or `limit_exceeded` past the closure bound.
   *As built (AC-1096):* a pending cross-file call's CALLS edge and Gap
   edge are both minted from the caller's parse with its decided receipts;
   completion keeps the one it emitted unchanged, on the same walk
   closure. A call resolved through a re-export, a base class or an
   interface is emitted with other files' evidence and keeps no receipt.
   *As built (AC-1098):* completion only edits held facts in place and
   appends new ones, so each fact it appends that no parse receipted is
   minted after completion against the TS file its evidence cites, and
   stated like that file's decided receipts. A TS receipt binds one file,
   so only facts whose evidence lies in one file qualify (Pulumi
   references, for example).
   *As built (AC-1520):* a TypeScript receipt whose ranges are all
   provenance may bind other TS/JS source files of its capture, as a lane
   receipt does (AC-1039), so a call resolved through a barrel, a base
   class or an interface is receipted on its caller's walk closure. A
   rule's inventory stays single-file.
   *As built (AC-1522):* such a receipt may also cite the walk's captured
   resolution configs (`package.json`, tsconfig, jsconfig), so a fact such
   as a Next API route, which cites `package.json`, is receipted too; a
   primary file is still a parsed source.
   *As built (AC-1524):* a parsed fact completion changes in place (a
   registered service worker's File and Symbols, marked `runtime`) loses
   its parse receipt, which no longer matches, and is minted afresh from
   the store like an emitted fact, binding the registering file. With
   that, each slice 6 fact family has a receipt wherever its evidence lies
   in captured sources and configs, within the receipt budget.
7. **External evidence identity** (`source-capture` manifest v2, registry,
   ingest manifest). Registration kind, capture kind, T1 provenance, graph
   schema bump. *Tests:* identities distinct across media, repository and
   manifest; same bytes as two media give two capture IDs; an over-limit
   trace gives `NotCaptured { limit_exceeded }` and emits no T1 fact; graph
   rebuild clears old T1 facts; no network.
8. **T1 enrichment receipts** (`iac`, `dynamic`). Receipts with `External`
   and `UpstreamFact` inputs. *Tests:* changing the state file changes the
   closure; an unestablished upstream gives `upstream_closure_incomplete`;
   T0 facts are byte-identical with and without enrichment (R-INT-1).
9. **Remaining producers**, one PR each: config and events, IaC HCL,
   declarative readers, ADR relinking. *Tests:* each declared input class
   changes the closure; each undeclared read fails a guard test; the slice 2
   fixtures are extended to the producer.
   *As built (AC-1526, IaC HCL):* Terraform receipts gain contract v4
   (`tf-primary-v2:`, `t0.adapter-iac/direct-lexical-v2`). Module
   expansion, policy-document grants and endpoint closure read across the
   whole walk, so every Terraform fact of a walk shares one closure: the
   capture's `Enumeration` under `tf-module-walk-v1` and each captured
   `.tf` file. The iac walk reports what it read besides the captured
   files (`SourceReads`); the closure is established only when it read no
   outside file, resolved every local module source, and each local module
   directory it listed in the checkout holds exactly the captured files
   directly in it. Otherwise the receipts are `cross_file_input_unbound`.
   A registry or remote module call makes them `remote_reference`. The
   walk reads no `*.tfvars`, so none is probed. Per-module scoping of the
   closure, so a remote call taints only its own module, is left for later.
   *As built (AC-1528, pipeline reader):* the first declarative reader
   gets receipts. A Pipeline receipt's grammar is chosen by the reader's
   own selection (`ingest::pipelines::is_pipeline_file`), with contract v4
   (`pipe-primary-v2:`, `t0.pipeline/declared-v2`) pinned to the YAML
   grammar. Over a capture the reader reads only the captured workflow
   files and Dockerfiles and probes no other location, so its facts share
   one closure: the capture's `Enumeration` under `pipeline-walk-v1` and
   each captured pipeline file.
   *As built (AC-1530, environment reader):* Environment receipts
   (`env-primary-v2:`, `t0.environments/declared-v2`) take the reader's
   selection less the files a TS source, config or other grammar claims.
   The reader lists the checkout for directories, named files and env
   files, so over a capture it runs again with the captured selection
   alone as listing and bytes; when that run opens nothing else and emits
   the same facts, they share one closure (the capture's `Enumeration`
   under `environment-walk-v1` and each captured environment file).
   Otherwise they are `cross_file_input_unbound`.
   *As built (AC-1532, schedule reader):* the first producer-keyed
   grammar. A schedule cites crontabs, YAML files and the scripts they
   run, whichever lane their paths select, so `Grammar::Schedule` is named
   by the producer and recorded in the receipt and its compact header;
   such a receipt may cite any captured source file but never an
   observation input. Contracts `sched-primary-v1:`/`sched-primary-v2:`
   (`t0.schedules/declared-v2`). Over a capture the reader lists and reads
   only the capture, so its closure is the capture's `Enumeration` under
   `schedule-walk-v1` and each captured file it read; a probed location
   the capture lacks is the enumeration's answer. A recognized Python
   dispatch enters a function only when the Python adapter emitted it, so
   the facts are established only when no dispatch asked for an adapter
   symbol, whatever the answer; otherwise `cross_file_input_unbound`.

Other language adapters and plugins stay `producer_not_participating`. No
slice switches a registration to Git-object production capture.

## Consequences

- A proposal can leave `awaiting_reconciliation` through an explicit,
  repeatable result. Its stored record, its ID and its history never change.
  A legacy record is never called verified. Only its corroboration at a
  recapture is recorded.
- "Input closure" becomes a checkable claim per fact, at the cost of a second
  persisted record and receipt contract v4. Most producers stay
  `not_established` for several slices. The UI and exports must keep showing
  that.
- Two identity changes need owner review. External T1 provenance changes fact
  bytes and the graph hash (slice 7), and the curated revision changes with
  policy v3 (slice 4). Tier, confidence and the T0 graph are otherwise
  untouched.
- New durable metadata: closure records (receipt store) and reconciliation
  results (`proposals.sqlite`). Both hold metadata only, follow existing
  bounds and follow source-owned retention.
- Questions for the owner:
  1. May `RecaptureCorroborated` items make a record eligible (recommended:
     yes, with separate counts)? Or should only `RetainedMatch` count?
  2. Should SPEC-12 eligibility also require an established closure?
     Recommended: not yet. Report it per item, and revisit after slice 6.
  3. Is the graph schema bump for external T1 provenance acceptable?

## Alternatives considered

1. **Rewrite `evidence_binding` and `context_status` in place after a
   successful recapture.** Rejected. SPEC-03 makes staged content immutable,
   and #385 forbids certifying old records from later bytes. A derived state
   over append-only results gives the same user outcome without touching
   history.
2. **Mark closure on the existing v1–v3 receipts once a producer proves it.**
   Rejected. The receipt ID hashes its content, so a closure attached later
   is a different claim about a past parse. New contracts carry it from
   minting.
3. **Treat external files as working-tree captures of the repository that
   declares them.** Rejected. That borrows the repository's identity and
   commit, which #385 forbids. It would also let one byte sequence pass as
   state and as a trace under one capture ID.
