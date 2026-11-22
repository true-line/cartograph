//! Pure reconciliation results for staged proposals (ADR-0072 decision 2,
//! slice 1, #385).
//!
//! A staged record never changes: its `evidence_binding`, `context_status`,
//! `proposal_id` and review revision stay as stored. Reconciliation instead
//! derives one immutable, metadata-only [`ReconciliationResultV1`] from exact
//! comparisons the host supplies after an explicit recapture and recovery of
//! the record's sources. This module has no graph, source-file, store or
//! provider API: it pins the comparison procedures, derives every item and
//! record outcome, and computes and checks the canonical result identity.

use crate::source_basis::{
    InputClosureStatus, TaskCaptureSpanRef, TaskEvidenceKind, TaskFactKey, TaskSourceBasisV2,
};
use crate::{AgentError, CandidateBasis, EvidenceBasis, StagedProposal};
use core_prov::content_hash;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Result interpretation version.
pub const RECONCILIATION_SCHEMA_VERSION: u32 = 1;
/// Identity prefix of a result id.
pub const RESULT_ID_PREFIX: &str = "reconcile-v1:";
/// Hash domain of a result id; the id field is excluded from its preimage.
const RESULT_DOMAIN: &[u8] = b"cartograph:reconciliation-result:v1\0";
/// Largest canonical result accepted by the decoder.
pub const MAX_RESULT_BYTES: usize = 64 * 1024;
/// Encoded size of `,"result_id":"reconcile-v1:<64 hex>"` in a stored result.
const RESULT_ID_FIELD_BYTES: usize = ",\"result_id\":\"\"".len() + RESULT_ID_PREFIX.len() + 64;
/// Most recaptured sources one result may name.
pub const MAX_RECOVERY_CAPTURES: usize = 64;
/// Most evidence or candidate items one result may report; a staged task
/// supplies at most 12 evidence spans and 8 candidates.
const MAX_ITEMS: usize = 64;
/// The frozen procedure that builds a candidate summary from a recovered node.
pub const CANDIDATE_SUMMARY_PROCEDURE: &str = "candidate-summary-v1";

/// `candidate-summary-v1`: the summary the host supplies for a candidate node,
/// `"{label}: {name}"`, where `name` is the node's `name`, else its `path`,
/// else its id. Unchanged since staging began (#384); task assembly calls this
/// so a later change has to be a new procedure version.
pub fn candidate_summary_v1(label: &str, props: &serde_json::Value, node_id: &str) -> String {
    let name = props["name"]
        .as_str()
        .or_else(|| props["path"].as_str())
        .unwrap_or(node_id);
    format!("{label}: {name}")
}

/// `legacy_text_v1`: the text hash a v1 working-tree read recorded for `bytes`
/// (slice, then lossy UTF-8, then hash). A recaptured span that is not strict
/// UTF-8 is `None`, because many byte strings decode to the same lossy text.
pub fn legacy_text_v1_hash(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes)
        .ok()
        .map(|text| content_hash(text.as_bytes()))
}

/// The recovery a result was compared against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryBasis {
    /// Durable recovery job.
    pub job_id: i64,
    /// Graph snapshot the comparison read.
    pub graph_snapshot_id: String,
    /// Recaptured sources, sorted and unique.
    pub captures: Vec<RecoveryCapture>,
}

/// One recaptured source of a recovery.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryCapture {
    /// Registered source id.
    pub source_id: String,
    /// Capture the recovery parsed.
    pub capture_id: String,
}

/// Which comparison a record's evidence uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileMethod {
    /// A v1 record: every item was a working-tree read.
    LegacyTextV1,
    /// A v2 record: items keep their own captured or working-tree origin.
    CapturedSpanV2,
}

/// Presence of a slot endpoint at the recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotPresence {
    /// Present unchanged.
    Present,
    /// No longer recovered.
    Missing,
}

/// State of the record's Gap at the recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapPresence {
    /// Still an unresolved Gap.
    Present,
    /// No longer recovered at all.
    Missing,
    /// Deterministic recovery now resolves the slot itself.
    GapResolved,
}

/// The directed slot the proposal fills: source, Gap and the relation between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotReconciliation {
    /// The slot's source node.
    pub source: SlotPresence,
    /// The slot's Gap node.
    pub gap: GapPresence,
    /// The source's relation to that Gap.
    pub relation: SlotPresence,
}

/// The original binding of one evidence item, copied from the staged record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OriginalBinding {
    /// A working-tree read with no attestation of the bytes it read.
    WorkingTreeUnverified,
    /// A captured primary-source range.
    Captured {
        /// Receipt the item was read through.
        receipt_id: String,
        /// Exact captured span.
        captured: TaskCaptureSpanRef,
    },
}

/// The receipt range at the recapture that a comparison used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciledOrigin {
    /// Current receipt.
    pub receipt_id: String,
    /// Range index in that receipt's inventory.
    pub receipt_inventory_index: u32,
    /// Exact recaptured span.
    pub captured: TaskCaptureSpanRef,
    /// What the current receipt says about its inputs.
    pub input_closure: InputClosureStatus,
}

/// Why an item changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangedReason {
    /// The recaptured span's bytes differ from the staged text.
    SpanBytesDiffer,
    /// The cited fact's digest differs from the staged selection.
    FactDigestDiffers,
}

/// Why an item is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingReason {
    /// The cited fact is no longer recovered.
    FactAbsent,
    /// The cited path is not in any recaptured source.
    PathNotCaptured,
    /// No current range covers the staged span exactly.
    SpanOutOfBounds,
}

/// Why an item cannot be compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnverifiableReason {
    /// The cited fact has no receipt from a participating producer.
    NoParticipatingReceipt,
    /// The recaptured span is not strict UTF-8, so a lossy text hash cannot match it.
    LossyText,
    /// The candidate summary procedure of the record is not known.
    SummaryProcedureUnknown,
    /// Validating the span would pass the captured validation byte bound.
    ValidationByteLimit,
}

/// The outcome of one evidence item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ItemOutcome {
    /// A captured item whose recaptured span and fact match exactly.
    RetainedMatch,
    /// A working-tree item whose recaptured bytes give the same text hash.
    RecaptureCorroborated,
    /// The item changed.
    Changed {
        /// Why.
        reason: ChangedReason,
    },
    /// The item is missing.
    Missing {
        /// Why.
        reason: MissingReason,
    },
    /// The item cannot be compared.
    Unverifiable {
        /// Why.
        reason: UnverifiableReason,
    },
}

/// One evidence item's reconciliation, in the staged record's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemReconciliation {
    /// Staged citation id.
    pub evidence_id: String,
    /// Staged binding.
    pub original: OriginalBinding,
    /// Comparison outcome.
    pub outcome: ItemOutcome,
    /// Present exactly for `retained_match` and `recapture_corroborated`.
    pub reconciled: Option<ReconciledOrigin>,
}

/// Why a candidate changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateChangedReason {
    /// The node's label differs.
    LabelDiffers,
    /// The rebuilt summary differs by hash or length.
    SummaryDiffers,
}

/// The outcome of one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CandidateOutcome {
    /// The rebuilt summary and label match.
    Matched,
    /// The candidate changed.
    Changed {
        /// Why.
        reason: CandidateChangedReason,
    },
    /// The candidate node is no longer recovered.
    Missing,
    /// The candidate cannot be compared.
    Unverifiable {
        /// Why.
        reason: UnverifiableReason,
    },
}

/// One candidate's reconciliation, in the staged record's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateReconciliation {
    /// Staged candidate node.
    pub node_id: String,
    /// Comparison outcome.
    pub outcome: CandidateOutcome,
}

/// A record's reconciliation outcome; precedence `changed` > `missing` >
/// `unverifiable` > `reconciled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationOutcome {
    /// Every item, candidate and slot endpoint matched.
    Reconciled,
    /// Something could not be compared, and nothing changed or went missing.
    Unverifiable,
    /// Something is missing, and nothing changed.
    Missing,
    /// Something changed.
    Changed,
}

/// One immutable, metadata-only reconciliation of a staged record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationResultV1 {
    /// Exactly 1.
    pub schema_version: u32,
    /// `reconcile-v1:` plus BLAKE3 of the canonical result without this field.
    pub result_id: String,
    /// The staged record, which binds its complete original content.
    pub proposal_id: String,
    /// The staged record's envelope version (1 or 2).
    pub proposal_schema_version: u32,
    /// The recovery compared against.
    pub recovery: RecoveryBasis,
    /// Comparison method, fixed by the envelope version.
    pub method: ReconcileMethod,
    /// The directed slot at the recovery.
    pub slot: SlotReconciliation,
    /// Exactly one per staged evidence item, in staged order.
    pub evidence: Vec<ItemReconciliation>,
    /// Exactly one per staged candidate, in staged order.
    pub candidates: Vec<CandidateReconciliation>,
    /// Derived record outcome.
    pub outcome: ReconciliationOutcome,
}

/// What the recovery shows for one staged evidence item, as the host found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecapturedItem<'a> {
    /// The cited fact is no longer recovered.
    FactAbsent,
    /// The cited fact has no participating receipt.
    NoParticipatingReceipt,
    /// The cited path is in no recaptured source.
    PathNotCaptured,
    /// No current range covers the staged span exactly.
    SpanOutOfBounds,
    /// Reading the span would pass the validation byte bound.
    ValidationByteLimit,
    /// The current range and its raw bytes.
    Range {
        /// The range the comparison uses.
        origin: ReconciledOrigin,
        /// Current digest of the cited fact.
        fact_digest: &'a str,
        /// Raw recaptured span bytes.
        bytes: &'a [u8],
    },
}

/// What the recovery shows for one staged candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecapturedCandidate {
    /// The node is no longer recovered.
    Absent,
    /// The node's current label and the summary rebuilt by
    /// [`candidate_summary_v1`].
    Present {
        /// Current label.
        label: String,
        /// Rebuilt summary.
        summary: String,
    },
}

/// Everything the host compared for one record.
#[derive(Debug, Clone)]
pub struct ReconciliationInput<'a> {
    /// The recovery compared against.
    pub recovery: RecoveryBasis,
    /// The directed slot at the recovery.
    pub slot: SlotReconciliation,
    /// One per staged evidence item, in staged order.
    pub evidence: Vec<RecapturedItem<'a>>,
    /// One per staged candidate, in staged order.
    pub candidates: Vec<RecapturedCandidate>,
}

fn invalid(what: &str) -> AgentError {
    AgentError::InvalidTask(format!("invalid reconciliation result: {what}"))
}

/// Compare a captured item: same fact digest, then BLAKE3 of the raw span
/// equals the staged text hash and its length equals the staged byte count.
pub fn compare_captured(
    staged: &EvidenceBasis,
    staged_fact_digest: &str,
    current: RecapturedItem<'_>,
) -> (ItemOutcome, Option<ReconciledOrigin>) {
    match common(current) {
        Err(outcome) => (outcome, None),
        Ok((origin, fact_digest, bytes)) => {
            if fact_digest != staged_fact_digest {
                (
                    ItemOutcome::Changed {
                        reason: ChangedReason::FactDigestDiffers,
                    },
                    None,
                )
            } else if bytes.len() != staged.text_bytes || content_hash(bytes) != staged.text_hash {
                (
                    ItemOutcome::Changed {
                        reason: ChangedReason::SpanBytesDiffer,
                    },
                    None,
                )
            } else {
                (ItemOutcome::RetainedMatch, Some(origin))
            }
        }
    }
}

/// Compare a working-tree item: the recaptured span must be strict UTF-8 and
/// hash, under `legacy_text_v1`, to the staged text hash. The staged
/// `EvidenceRef.commit_sha` is never consulted.
pub fn compare_working_tree(
    staged: &EvidenceBasis,
    current: RecapturedItem<'_>,
) -> (ItemOutcome, Option<ReconciledOrigin>) {
    match common(current) {
        Err(outcome) => (outcome, None),
        Ok((origin, _, bytes)) => match legacy_text_v1_hash(bytes) {
            None => (
                ItemOutcome::Unverifiable {
                    reason: UnverifiableReason::LossyText,
                },
                None,
            ),
            Some(hash) if hash == staged.text_hash && bytes.len() == staged.text_bytes => {
                (ItemOutcome::RecaptureCorroborated, Some(origin))
            }
            Some(_) => (
                ItemOutcome::Changed {
                    reason: ChangedReason::SpanBytesDiffer,
                },
                None,
            ),
        },
    }
}

fn common(current: RecapturedItem<'_>) -> Result<(ReconciledOrigin, &str, &[u8]), ItemOutcome> {
    let missing = |reason| Err(ItemOutcome::Missing { reason });
    let unverifiable = |reason| Err(ItemOutcome::Unverifiable { reason });
    match current {
        RecapturedItem::FactAbsent => missing(MissingReason::FactAbsent),
        RecapturedItem::PathNotCaptured => missing(MissingReason::PathNotCaptured),
        RecapturedItem::SpanOutOfBounds => missing(MissingReason::SpanOutOfBounds),
        RecapturedItem::NoParticipatingReceipt => {
            unverifiable(UnverifiableReason::NoParticipatingReceipt)
        }
        RecapturedItem::ValidationByteLimit => {
            unverifiable(UnverifiableReason::ValidationByteLimit)
        }
        RecapturedItem::Range {
            origin,
            fact_digest,
            bytes,
        } => {
            let span = origin
                .captured
                .byte_end
                .checked_sub(origin.captured.byte_start);
            if span != Some(bytes.len() as u64) {
                return missing(MissingReason::SpanOutOfBounds);
            }
            Ok((origin, fact_digest, bytes))
        }
    }
}

/// Compare a candidate under `candidate-summary-v1`, by label, hash and length.
pub fn compare_candidate(
    staged: &CandidateBasis,
    current: &RecapturedCandidate,
) -> CandidateOutcome {
    match current {
        RecapturedCandidate::Absent => CandidateOutcome::Missing,
        RecapturedCandidate::Present { label, summary } => {
            if *label != staged.label {
                CandidateOutcome::Changed {
                    reason: CandidateChangedReason::LabelDiffers,
                }
            } else if summary.len() != staged.summary_bytes
                || content_hash(summary.as_bytes()) != staged.summary_hash
            {
                CandidateOutcome::Changed {
                    reason: CandidateChangedReason::SummaryDiffers,
                }
            } else {
                CandidateOutcome::Matched
            }
        }
    }
}

impl ItemOutcome {
    fn record(self) -> ReconciliationOutcome {
        match self {
            Self::RetainedMatch | Self::RecaptureCorroborated => ReconciliationOutcome::Reconciled,
            Self::Changed { .. } => ReconciliationOutcome::Changed,
            Self::Missing { .. } => ReconciliationOutcome::Missing,
            Self::Unverifiable { .. } => ReconciliationOutcome::Unverifiable,
        }
    }
    fn reconciles(self) -> bool {
        matches!(self, Self::RetainedMatch | Self::RecaptureCorroborated)
    }
}

impl CandidateOutcome {
    fn record(self) -> ReconciliationOutcome {
        match self {
            Self::Matched => ReconciliationOutcome::Reconciled,
            Self::Changed { .. } => ReconciliationOutcome::Changed,
            Self::Missing => ReconciliationOutcome::Missing,
            Self::Unverifiable { .. } => ReconciliationOutcome::Unverifiable,
        }
    }
}

impl SlotReconciliation {
    fn record(self) -> ReconciliationOutcome {
        let presence = |value| match value {
            SlotPresence::Present => ReconciliationOutcome::Reconciled,
            SlotPresence::Missing => ReconciliationOutcome::Missing,
        };
        let gap = match self.gap {
            GapPresence::Present => ReconciliationOutcome::Reconciled,
            GapPresence::Missing => ReconciliationOutcome::Missing,
            GapPresence::GapResolved => ReconciliationOutcome::Changed,
        };
        presence(self.source).max(gap).max(presence(self.relation))
    }
}

/// One staged evidence item, its original binding and, for a v2 record, the
/// staged digest of the fact it cites.
type StagedItem<'a> = (&'a EvidenceBasis, OriginalBinding, Option<&'a str>);

/// The staged evidence items of `proposal`, in staged order.
fn staged_items(proposal: &StagedProposal) -> Result<Vec<StagedItem<'_>>, AgentError> {
    let Some(basis) = &proposal.source_basis else {
        return Ok(proposal
            .basis
            .evidence
            .iter()
            .map(|item| (item, OriginalBinding::WorkingTreeUnverified, None))
            .collect());
    };
    proposal
        .basis
        .evidence
        .iter()
        .map(|item| {
            let origin = basis
                .evidence
                .iter()
                .find(|origin| origin.evidence_id == item.id)
                .ok_or_else(|| invalid("evidence origin"))?;
            let digest = fact_digest(basis, &origin.fact)?;
            let original = match &origin.origin {
                TaskEvidenceKind::WorkingTreeUnverified => OriginalBinding::WorkingTreeUnverified,
                TaskEvidenceKind::CapturedPrimarySource {
                    receipt_id,
                    captured,
                    ..
                } => OriginalBinding::Captured {
                    receipt_id: receipt_id.clone(),
                    captured: captured.clone(),
                },
            };
            Ok((item, original, Some(digest)))
        })
        .collect()
}

fn fact_digest<'a>(
    basis: &'a TaskSourceBasisV2,
    fact: &TaskFactKey,
) -> Result<&'a str, AgentError> {
    basis
        .selected_facts
        .iter()
        .find(|selection| selection.fact == *fact)
        .map(|selection| selection.fact_digest.as_str())
        .ok_or_else(|| invalid("evidence fact"))
}

impl ReconciliationResultV1 {
    /// Derive the result for `proposal` from the host's comparisons. Every item
    /// and candidate outcome is reported, in staged order.
    pub fn derive(
        proposal: &StagedProposal,
        input: ReconciliationInput<'_>,
    ) -> Result<Self, AgentError> {
        let items = staged_items(proposal)?;
        if input.evidence.len() != items.len()
            || input.candidates.len() != proposal.basis.candidates.len()
        {
            return Err(invalid("item count"));
        }
        let evidence = items
            .into_iter()
            .zip(input.evidence)
            .map(|((staged, original, digest), current)| {
                let (outcome, reconciled) = match (&original, digest) {
                    (OriginalBinding::Captured { .. }, Some(digest)) => {
                        compare_captured(staged, digest, current)
                    }
                    (OriginalBinding::Captured { .. }, None) => {
                        return Err(invalid("captured digest"));
                    }
                    (OriginalBinding::WorkingTreeUnverified, _) => {
                        compare_working_tree(staged, current)
                    }
                };
                Ok(ItemReconciliation {
                    evidence_id: staged.id.clone(),
                    original,
                    outcome,
                    reconciled,
                })
            })
            .collect::<Result<Vec<_>, AgentError>>()?;
        let candidates = proposal
            .basis
            .candidates
            .iter()
            .zip(&input.candidates)
            .map(|(staged, current)| CandidateReconciliation {
                node_id: staged.node_id.clone(),
                outcome: compare_candidate(staged, current),
            })
            .collect();
        let method = match proposal.source_basis {
            None => ReconcileMethod::LegacyTextV1,
            Some(_) => ReconcileMethod::CapturedSpanV2,
        };
        let mut result = Self {
            schema_version: RECONCILIATION_SCHEMA_VERSION,
            result_id: String::new(),
            proposal_id: proposal.proposal_id.clone(),
            proposal_schema_version: proposal.schema_version,
            recovery: input.recovery,
            method,
            slot: input.slot,
            evidence,
            candidates,
            outcome: ReconciliationOutcome::Reconciled,
        };
        result.outcome = result.derived_outcome();
        result.result_id = result.compute_id()?;
        result.validate()?;
        Ok(result)
    }

    /// The record outcome its parts give, under the fixed precedence.
    pub fn derived_outcome(&self) -> ReconciliationOutcome {
        self.evidence
            .iter()
            .map(|item| item.outcome.record())
            .chain(self.candidates.iter().map(|item| item.outcome.record()))
            .fold(self.slot.record(), Ord::max)
    }

    /// Canonical identity: BLAKE3 over the domain and the canonical JSON of the
    /// result without `result_id` (sorted keys, no whitespace).
    pub fn compute_id(&self) -> Result<String, AgentError> {
        let mut value = serde_json::to_value(self)?;
        value
            .as_object_mut()
            .ok_or_else(|| invalid("shape"))?
            .remove("result_id");
        let bytes = serde_json::to_vec(&canonical(value))?;
        // Leave room for the id field, so every result this module accepts
        // still decodes once its id is stored with it.
        if bytes.len() + RESULT_ID_FIELD_BYTES > MAX_RESULT_BYTES {
            return Err(invalid("size"));
        }
        let mut input = RESULT_DOMAIN.to_vec();
        input.extend(bytes);
        Ok(format!("{RESULT_ID_PREFIX}{}", content_hash(&input)))
    }

    /// Canonical stored bytes (sorted keys, no whitespace), id included.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, AgentError> {
        Ok(serde_json::to_vec(&canonical(serde_json::to_value(self)?))?)
    }

    /// Strict decode: unknown fields, a wrong id, a derived outcome that
    /// differs from the stored one, or any broken invariant is rejected.
    pub fn decode(bytes: &[u8]) -> Result<Self, AgentError> {
        if bytes.len() > MAX_RESULT_BYTES {
            return Err(invalid("size"));
        }
        let result: Self = serde_json::from_slice(bytes).map_err(|_| invalid("encoding"))?;
        result.validate()?;
        Ok(result)
    }

    /// Check this result against the staged record it names: the same id and
    /// envelope version, and one item per staged item with the staged binding,
    /// in staged order.
    pub fn validate_for(&self, proposal: &StagedProposal) -> Result<(), AgentError> {
        let items = staged_items(proposal)?;
        if self.proposal_id != proposal.proposal_id
            || self.proposal_schema_version != proposal.schema_version
            || self.method
                != if proposal.source_basis.is_none() {
                    ReconcileMethod::LegacyTextV1
                } else {
                    ReconcileMethod::CapturedSpanV2
                }
            || self.evidence.len() != items.len()
            || self.candidates.len() != proposal.basis.candidates.len()
            || self
                .evidence
                .iter()
                .zip(&items)
                .any(|(item, (staged, original, _))| {
                    item.evidence_id != staged.id || item.original != *original
                })
            || self
                .candidates
                .iter()
                .zip(&proposal.basis.candidates)
                .any(|(item, staged)| item.node_id != staged.node_id)
        {
            return Err(invalid("proposal binding"));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), AgentError> {
        let r = &self.recovery;
        let prefixed = |value: &str, prefix: &str| {
            value
                .strip_prefix(prefix)
                .is_some_and(crate::source_basis::hash)
        };
        if self.schema_version != RECONCILIATION_SCHEMA_VERSION
            || !matches!(self.proposal_schema_version, 1 | 2)
            || (self.method == ReconcileMethod::LegacyTextV1) != (self.proposal_schema_version == 1)
            || !crate::source_basis::text(&self.proposal_id, 256)
            || r.job_id <= 0
            || !prefixed(&r.graph_snapshot_id, "context-v1:")
            || r.captures.is_empty()
            || r.captures.len() > MAX_RECOVERY_CAPTURES
            || r.captures.windows(2).any(|pair| pair[0] >= pair[1])
            || r.captures.iter().any(|capture| {
                !crate::source_basis::text(&capture.source_id, 256)
                    || !crate::source_basis::text(&capture.capture_id, 256)
            })
            || self.evidence.len() > MAX_ITEMS
            || self.candidates.len() > MAX_ITEMS
        {
            return Err(invalid("fields"));
        }
        for item in &self.evidence {
            let legacy_captured = self.method == ReconcileMethod::LegacyTextV1
                && matches!(item.original, OriginalBinding::Captured { .. });
            let wrong_match = matches!(
                (&item.original, item.outcome),
                (
                    OriginalBinding::Captured { .. },
                    ItemOutcome::RecaptureCorroborated
                ) | (
                    OriginalBinding::WorkingTreeUnverified,
                    ItemOutcome::RetainedMatch
                )
            );
            if !crate::source_basis::text(&item.evidence_id, 256)
                || legacy_captured
                || wrong_match
                || item.outcome.reconciles() != item.reconciled.is_some()
                || item
                    .reconciled
                    .as_ref()
                    .is_some_and(|origin| !self.origin_in_recovery(origin))
            {
                return Err(invalid("evidence item"));
            }
        }
        if self
            .candidates
            .iter()
            .any(|item| !crate::source_basis::text(&item.node_id, 8192))
            || self.outcome != self.derived_outcome()
            || self.result_id != self.compute_id()?
        {
            return Err(invalid("derived fields"));
        }
        Ok(())
    }
}

impl ReconciliationResultV1 {
    /// A matched item's origin must be a well-formed captured span of one of
    /// the captures the recovery names.
    fn origin_in_recovery(&self, origin: &ReconciledOrigin) -> bool {
        let span = &origin.captured;
        let file = &span.file;
        crate::source_basis::text(&origin.receipt_id, 256)
            && crate::source_basis::text(&file.path, 4096)
            && file
                .capture_id
                .strip_prefix("capture-v1:")
                .is_some_and(crate::source_basis::hash)
            && crate::source_basis::hash(&file.digest)
            && span.byte_start < span.byte_end
            && span.byte_end <= file.byte_len
            && self.recovery.captures.iter().any(|capture| {
                capture.source_id == file.source_id && capture.capture_id == file.capture_id
            })
    }
}

fn canonical(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical).collect())
        }
        other => other,
    }
}

#[cfg(test)]
#[path = "reconciliation_tests.rs"]
mod tests;
