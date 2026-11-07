# Text-free refusal diagnostics (#1707)

Status: A is an inert diagnostic library; B host integration follows after A merges. Base: 68f6a74588a68de071759e0eb18155d0880d0461.

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

Add an optional versioned diagnostic field to the staged refusal record and the
withholding witness. Missing fields keep historical serialized bytes and reads
unchanged. New host records include diagnostics before computing their existing
content identity and independent witness. Do not add fields to RefusalFeedback.

Compute diagnostics while the exact authored finish and authenticated cited spans
are available. Bind them to that finish's existing hash and response authority.
Final withholding must describe the current proposal, not copy the first
proposal's correction locations. Retain a reason for each withheld index.

Use existing policy/record byte ceilings; do not raise numeric limits. Bounded
retention records omitted entries/tokens explicitly while detection continues.
A classification failure cannot turn a refusal into admission or weaken an
existing witness check. Old records without diagnostics remain readable.

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
