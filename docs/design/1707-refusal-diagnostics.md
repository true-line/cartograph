# Text-free refusal diagnostics (#1707)

Status: A merged in #1708 at f1826da3533338ae813a1a2dc785e99e1fd50eba. B implements the owner-approved separate sidecar contract below.

## Contract

Classify the existing enumeration/configuration verdict without changing which
finish is refused or withheld. Preserve model-facing feedback serialization,
request bytes, specialist versions, prompts, policy limits and privacy rules.
The new data is host-side diagnostic evidence, never a model instruction.

## Detector result

Replace ambiguous internal boolean construction with a typed explanation and
retain the historical boolean adapter. Distinguish a contradicted supported
relation from unsupported grammar and a fixed-vocabulary qualifier. Preserve
first-match order and the complete visitor's order. Configuration checks record
why the existing witness search failed; diagnostics must not claim that unknown
syntax proves a real binding is absent.

Each diagnostic identifies the finding field and sentence, cited evidence ID,
literal table kind, quantifier, attempted grammar rule, matched-identifier count,
and source-table positions. Positions are zero-based key/value indexes into the
same parsed cited literal, never identifier strings. A false relation includes
the counterexample key/target positions needed to recompute the verdict.

Token shape uses a closed vocabulary of grammar words and typed word, number,
punctuation and identifier-position symbols. Unknown tokens cannot become
serialized literals. Shape follows the detector's actual tokenizer; punctuation
that the detector discards is not invented as a token. Duplicate identifiers can
map to multiple positions; retain deterministic source order rather than silently
choosing an unrelated occurrence.

## Storage and integrity

Store diagnostics in a separate text-free host sidecar per refused finish, outside
`STORE_CAPACITY` and workspace accounting. The owner approved this exception on
2026-11-07 after the capacity analysis showed that adding retained bytes to the
existing store can change a later exact-fit admission. Do not embed diagnostic
payloads in `RefusalRecord`, `RefusalFeedback`, or the model-facing input.

The sole new numeric policy is `MAX_REFUSAL_DIAGNOSTIC_BYTES = 4096`, measured on
the complete serialized sidecar, including identity and omission metadata. No
other numeric limit changes. Retain deterministic prefixes with explicit omitted
entry/token/position counts. If details do not fit, retain an explicit text-free
`unrecorded` capacity marker; absence is reserved for historical records.

Bind each sidecar to the refusal record and authored finish hashes and cover its
integrity with the existing independent witness. Reopen must reject changed,
substituted, or missing required sidecars, including when a local content hash is
recomputed. Historical records must reopen without rewriting their saved bytes.
The storage transaction must keep the refusal and its sidecar consistent.

Compute diagnostics while the exact authored finish and authenticated cited spans
are available. Final withholding must describe the current proposal, not copy
the first proposal's correction locations; retain a reason for each withheld
index. Diagnostic collection does not change detection, admission, or feedback.

Whole-run invariance covers disposition, withholding, model-facing bytes, later
task/global capacity, and provider dispatch counts with diagnostics retained or
without room for details. The sidecar's serialized cap is distinct from the
classifier's token/position allocation bounds. Transient collection and witness
regeneration must remain bounded even when the retained representation is small.

## Host integration plan

The current host creates `RefusalRecord` in `jobs/investigations/staged.rs`,
then sets the fixed-length `finish_feedback_witness` in the independently stored
response. Bind the sidecar through a domain-separated digest in that existing
witness slot, rather than adding a charged response field. Preserve the original
refusal identity and feedback serialization. Prove that all affected encoded
response and journal lengths remain equal to their baseline lengths.

Use a separate validated SQL namespace: the existing `staged_completion_*`
schema validator rejects extra objects. Sidecar rows need immutable triggers,
canonical schema validation, bounded reads before deserialization, and insertion
in the refusal transaction. Exclude only the new sidecar namespace from existing
occupancy accounting; do not alter how any existing row is charged.

Replay currently validates the feedback witness without a connection. Extend the
host validation path to load the sidecar before checking the digest, including
the independent response reload in `admission_journal.rs`. A missing sidecar must
not cause a fallback that accepts a new digest as historical. Legacy digests must
remain validated by their original rule. Test deletion, substitution, oversized
bodies, malformed schema, and rehashed payload tampering.

The delegate confirmed scope on 2026-11-07: only new records of analyst roles
22–25 receive diagnostics. Earlier roles remain unchanged; future roles adopt
the independent witness through their own version bump. Current-finish withholding also has a separate
witness path and must be covered explicitly, not inferred from refusal tests.
Its sidecar digest belongs in the independently stored response witness slot,
not in `FindingWithholdingWitness`: the latter contributes to the semantic
auditor's candidate hash. Keep the candidate and its withholding witness byte
identical regardless of retained diagnostics, and domain-bind the sidecar with
the unchanged witness fingerprint in response metadata. Validate both bindings
on reopen, including while a semantic audit candidate is pending.


## Evidence and review surfaces

Development receipts expose the host field and summaries report per-refusal
reason codes. Audit/diagnostic projections may display closed codes, positions
and token classes, but never refused prose or source identifiers. Verify model
prompt construction excludes the host field, including subsequent corrections
and separate auditor preparation.

## Verification

- Cover every enum/configuration reason branch, including r3 fixtures.
- Pin exact token shapes/source positions; reconstruct false relations from
  fixture tables and recorded positions.
- Compare unchanged detector decisions and @20–@25 model feedback bytes/hashes.
- Exercise duplicate identifiers, non-ASCII text, unsupported qualifiers,
  truncation, and malicious unknown literal attempts.
- Read historical refusal/witness records; reject tampered new diagnostics via
  their independently bound host witness.
- Exercise current-finish withholding reasons through settlement and reopen.
- Run the repository's complete local gates; Claude owns independent review,
  signed publication and current-head CI.

Bare-field-name limitations, wording quality, grammar expansion, new specialist
versions, provider retries and another development run are outside this change.
