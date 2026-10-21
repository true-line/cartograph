# ADR-0038 — Independent evaluation for semantic families

- **Status:** Proposed
- **Date:** 2026-10-21
- **Deciders:** Cartograph maintainers (owner review pending)

## Context

The existing semantic evaluation chooses a threshold and reports precision and
recall on the same labeled pairs. Its global `passed` value then admits both
call and channel proposals. That measures calibration, not independent quality,
and does not establish that one family’s operating point applies to another.
Historical rows must remain readable without gaining authority retrospectively.

## Decision

Use the exact `(HopKind, subtype)` candidate-group key throughout proposal
construction, evaluation and admission. A null subtype is a distinct family,
not a wildcard. Preserve staged proposals when a family cannot be admitted.
Only admitted Gap fills enter the ephemeral inferred overlay; confirmed facts
are never overwritten.

Freeze source inputs, complete eligible candidate sets, labels, leakage groups
and partition identity before selecting an operating point. Unknown labels are
not negatives. Missing edges, shuffled destinations and name similarity alone
do not establish negative truth. Source evidence or recorded adjudication must
support no-match and hard-negative labels. Hashes prove consistency, not that a
label is correct or independently authored.

Keep calibration inputs and labels separate from evaluation inputs. Prediction
uses the real proposal construction and ranking path under a frozen policy;
evaluation labels are consumed only after predictions are sealed. Thresholds,
ambiguity margins, candidate limits and minimum-case requirements are explicit
versioned policy inputs. Equal or insufficiently separated candidates abstain.
Do not lower adequacy requirements or tune thresholds after inspecting results.

The independent partition rule is `catalogue-leakage@1`. A complete unlabelled
retrieval catalogue may be shared across partitions; catalogue membership alone
joins no cases and supplies no negative label. Repeated supervised occurrences
of a target (positive or explicit hard negative), identical queries, query/source
spans and bodies, and label-origin evidence must remain in one declared group.
Each group's related sources are exactly its hop and label-origin sources plus
sources of candidates explicitly labelled by those cases. Validate this against
labels after loading; the leakage document itself stays label-free, avoiding a
hash cycle. Its rule version and digest enter the policy commitment. Salt and
group assignments are frozen before calibration, with no outcome-driven search.
The published visible fixture rule keeps its original `sha256-group-bucket@1`
version and stricter all-candidate grouping. Purpose/version mismatches reject.

Bind receipts to the dataset, label and partition digests, family, candidate
recipe, frozen policy and the observed embedding-space binding from ADR-0037.
Reuse its namespace derivation and observed dimension after the operation is
sealed and its provider identity and lifecycle lease are rechecked. A mutable
model tag or caller-supplied model name is insufficient. Changed or unavailable
bindings yield explicit abstention.

Distinguish three report purposes: calibration only, visible mechanism
evaluation and independent evaluation. Public scripted fixtures test mechanisms
and can never become real-quality evidence by meeting a metric. Production
admission accepts a distinct independent receipt type constructed only through
host-side validation and scoring. Receipt types are not deserializable grants;
stored reports and caller-supplied flags, counts or digests are descriptive data.

The host loads an independent reviewed-dataset envelope from its controlled
evaluation store, checks its frozen policy commitment and dataset references,
and requires a reviewer distinct from the label author and recorded policy
implementer. Reviewer identity strings remain claims, not authentication.
The host anchor selects a retained capture explicitly. Graph identity comes
from the actual bounded current projection and capture identity from validated
retained bytes. This reviewed association is not a retrospective producer
receipt: ordinary Gap, File and call facts have no such receipt. Dataset spans
must match retained bytes, and dataset queries and complete candidate sets must
match the actual graph-derived inputs.

Reloading a report does not restore authority: revalidate its envelope and
current corpus/model bindings. Missing, changed, incompatible or insufficient
evidence abstains. Operational blindness requires an independently controlled
store and runner; hidden files, public fixture commits and shared agent
filesystems do not provide it.

Persist bounded evaluation summaries atomically with their scalar records.
Legacy rows remain visibly calibration-only, regardless of their old `passed`
value. The existing Provenance & Eval surface reports each family’s status,
reason and staged/admitted counts, and labels mechanism fixtures explicitly.

## Consequences

A fresh application without compatible independent evidence will retain staged
proposals and report abstention. It will not manufacture quality labels to make
an unattended run appear accepted. This is a deliberate evidence boundary.

Independent-author visible fixtures exercise serialization, leakage rejection,
fixed-policy scoring, identity compatibility, persistence and UI behavior with
scripted providers. They do not establish real-model precision or fresh-system
acceptance. Parent #258 stays open for independently reviewed real data and the
separately authorized full-repository experiment. This decision authorizes no
model download, source download or live-provider run.

## Alternatives (≤3)

- **Reuse the calibration `passed` flag:** cannot establish independent quality
  or cross-family applicability.
- **Trust an imported receipt’s metrics:** permits unchecked client data to
  grant admission and cannot detect changed evidence on reload.
- **Call public fixtures a blind holdout:** confuses API separation with actual
  isolation and permits tuning to visible answers.
