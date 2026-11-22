//! Staged finish decisions only: no provider, public result or task transition.
use super::{obligations::*, staged_attempt::*, *};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionOutcome {
    Supported,
    /// Host-only disposition; StagedFinish decoding rejects this outcome.
    FindingsWithheldAfterCorrection,
    InsufficientEvidence,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedFinish {
    pub schema_version: u32,
    pub outcome: CompletionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrative: Option<InvestigationNarrative>,
    pub action: InvestigationAction,
}
impl StagedFinish {
    pub fn decode(raw: &str) -> Result<Self, InvestigationError> {
        Self::decode_detailed(raw).map_err(|failure| failure.error)
    }
    pub(super) fn decode_detailed(raw: &str) -> Result<Self, ActionDecodeFailure> {
        use ActionDecodeStage as Stage;
        strict::preflight(raw, MAX_ACTION_BYTES, 32, 4096, 32)
            .map_err(|error| Stage::Preflight.failure(error))?;
        let finish: Self =
            serde_json::from_str(raw).map_err(|_| Stage::TypedStagedFinish.invalid())?;
        finish.validate_for_role_detailed(false)?;
        if serde_json::to_value(&finish).map_err(|_| Stage::CanonicalFields.invalid())?
            != serde_json::from_str::<serde_json::Value>(raw)
                .map_err(|_| Stage::TypedEnvelope.invalid())?
        {
            return Err(Stage::CanonicalFields.invalid());
        }
        Ok(finish)
    }
    pub fn decode_message(raw: &str) -> Result<Self, InvestigationError> {
        strict::preflight(raw, MAX_ACTION_BYTES, 32, 4096, 32)?;
        let finish: Self =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidAction)?;
        finish.validate_for_role(true)?;
        let original: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidAction)?;
        if serde_json::to_value(&finish).map_err(|_| InvestigationError::InvalidAction)? != original
        {
            return Err(InvestigationError::InvalidAction);
        }
        Ok(finish)
    }
    fn validate_for_role(&self, message: bool) -> Result<(), InvestigationError> {
        self.validate_for_role_detailed(message)
            .map_err(|failure| failure.error)
    }
    fn validate_for_role_detailed(&self, message: bool) -> Result<(), ActionDecodeFailure> {
        if self.schema_version != if message { 2 } else { 1 }
            || (!message && self.narrative.is_some())
            || (message
                && (self.outcome == CompletionOutcome::Supported) != self.narrative.is_some())
        {
            return Err(ActionSemanticRule::FinishRole.invalid());
        }
        let action = InvestigationAction::decode_detailed(
            &String::from_utf8(
                bounded_bytes(&self.action, MAX_ACTION_BYTES)
                    .map_err(|error| ActionSemanticRule::FinishShape.failure(error))?,
            )
            .map_err(|_| ActionSemanticRule::FinishShape.invalid())?,
        )
        .map_err(nested_finish_failure)?;
        match (&action, self.outcome) {
            (
                InvestigationAction::Finish {
                    findings,
                    knowledge_completeness,
                    ..
                },
                CompletionOutcome::Supported,
            ) if !findings.is_empty()
                && *knowledge_completeness != KnowledgeCompleteness::InsufficientEvidence =>
            {
                Ok(())
            }
            (
                InvestigationAction::Finish {
                    findings,
                    knowledge_completeness,
                    ..
                },
                CompletionOutcome::InsufficientEvidence,
            ) if findings.is_empty()
                && *knowledge_completeness == KnowledgeCompleteness::InsufficientEvidence =>
            {
                Ok(())
            }
            (InvestigationAction::Finish { .. }, _) => {
                Err(ActionSemanticRule::FinishOutcome.invalid())
            }
            _ => Err(ActionSemanticRule::FinishRole.invalid()),
        }
    }
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        self.validate_for_role(self.schema_version == 2)?;
        Ok(crate::source_basis::domain_hash(
            if self.schema_version == 2 {
                b"cartograph:obligation-finish:v2\0"
            } else {
                b"cartograph:obligation-finish:v1\0"
            },
            &bounded_bytes(self, MAX_ACTION_BYTES)?,
        ))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    NotAttempted,
    ExcludedByCapacity,
    AcquiredNotSupplied,
    Redacted,
    HashMismatch,
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackEntry {
    pub obligation_id: String,
    pub kind: String,
    pub reason: RefusalReason,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProseCorrection {
    Combined(Box<CombinedCorrections>),
    RewriteSchemaPlaceholdersAsPlainProse,
    ParaphraseWithoutQuotingSource,
    ParaphraseReplayAt(ReplayLocation),
    UnverifiedEnumerationClaimAt(ReplayLocation),
    UnverifiedConfigurationClaimAt(ReplayLocation),
    LimitationDeniesAcquiredEvidence(Box<ConflictingEvidence>),
}

#[path = "closed_feedback_reserve.rs"]
mod closed_feedback_reserve;
#[path = "combined_feedback.rs"]
mod combined_feedback;
pub use combined_feedback::{CombinedCorrection, CombinedCorrections};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConflictingEvidence {
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefusalFeedback {
    /// Fixed correction instruction; rejected authored text is never echoed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prose_correction: Option<ProseCorrection>,
    pub schema_version: u32,
    /// Version two stores exact citation guidance. Absent for every historical
    /// version-one record, preserving its canonical bytes and fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation_correction: Option<acquisition_choices::CitationCorrection>,
    pub unmet: Vec<FeedbackEntry>,
}

/// Synthetic serialization envelope for sizing only, never a real refusal,
/// assessment, completed attempt or authorization to dispatch a provider.
///
/// It serializes exactly as `RefusalFeedback`, with no additional wrapper.
/// The private representation has no conversion into authoritative feedback.
/// Its bound covers this component only, not a complete prompt or payload.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct RefusalFeedbackSizeEnvelope {
    feedback: RefusalFeedback,
}
/// Conservative serializer-only envelope for version-two feedback and its
/// projected correction. Never decodable as authoritative feedback.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct TypedFeedbackSizeEnvelope(String);

/// Feedback entries are at most 137 encoded bytes (a 64-hex id, `inventory`,
/// `acquired_not_supplied`); the frame is 31. These allowances keep v1's
/// literal 2 KiB bound for its 12 obligations and scale with the saved policy.
const FEEDBACK_ENTRY_BYTES: usize = 160;
const FEEDBACK_FRAME_BYTES: usize = 128;

fn feedback_bytes(policy: &InvestigationPolicy) -> Result<usize, InvestigationError> {
    policy
        .max_completion_obligations()
        .checked_mul(FEEDBACK_ENTRY_BYTES)
        .and_then(|bytes| bytes.checked_add(FEEDBACK_FRAME_BYTES))
        .ok_or(InvestigationError::LimitExceeded)
}

impl RefusalFeedback {
    /// Any admitted complete feedback has at most B encoded UTF-8 bytes.
    /// B literal backslashes serialize to 2B+2 bytes; every further JSON-string
    /// embedding at most doubles each already encoded byte. Thus this string
    /// dominates every feedback at every enclosing escape layer, with other
    /// fields unchanged, including escaped actions and multibyte identities.
    /// It contains no source/secret recognizer text. This is deliberately a
    /// sizing value, not a new feedback protocol or a larger admission limit.
    pub fn sizing_typed_upper_envelope_with_policy(
        policy: &InvestigationPolicy,
    ) -> Result<TypedFeedbackSizeEnvelope, InvestigationError> {
        Ok(TypedFeedbackSizeEnvelope(
            "\\".repeat(feedback_bytes(policy)?),
        ))
    }

    /// The fixed contract bounds every possible query/read correction; the
    /// unchanged total admission ceiling is still the fallback upper bound.
    pub(super) fn sizing_typed_contract_envelope(
        contract: &InvestigationCompletionContract,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<TypedFeedbackSizeEnvelope, InvestigationError> {
        Ok(TypedFeedbackSizeEnvelope("\\".repeat(
            acquisition_choices::feedback_encoded_bound(contract, ledger, policy)?,
        )))
    }

    pub(super) fn sizing_replay_contract_envelope(
        contract: &InvestigationCompletionContract,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<TypedFeedbackSizeEnvelope, InvestigationError> {
        let mut feedback =
            Self::sizing_upper_envelope_with_policy(&contract.obligations, policy)?.feedback;
        feedback.schema_version = 4;
        // FindingLimitation is the longest field encoding; the largest valid
        // indices and sentence ordinal dominate every location at each layer.
        let position = ProsePosition {
            field: ProseField::FindingLimitation {
                finding: 11,
                limitation: 7,
            },
            sentence: MAX_INPUT_BYTES as u32,
        };
        feedback.prose_correction = Some(ProseCorrection::ParaphraseReplayAt(ReplayLocation {
            start: position.clone(),
            end: position,
        }));
        let cap = feedback_bytes(policy)?;
        let replay = bounded_bytes(&feedback, cap).map_or(cap, |bytes| bytes.len());
        Ok(TypedFeedbackSizeEnvelope("\\".repeat(replay.max(
            acquisition_choices::feedback_encoded_bound(contract, ledger, policy)?,
        ))))
    }

    pub(super) fn sizing_claim_contract_envelope(
        contract: &InvestigationCompletionContract,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<TypedFeedbackSizeEnvelope, InvestigationError> {
        let mut feedback =
            Self::sizing_upper_envelope_with_policy(&contract.obligations, policy)?.feedback;
        feedback.schema_version = 5;
        // FindingLimitation is the longest field encoding; the largest valid
        // indices and sentence ordinal dominate every location at each layer.
        let position = ProsePosition {
            field: ProseField::FindingLimitation {
                finding: 11,
                limitation: 7,
            },
            sentence: MAX_INPUT_BYTES as u32,
        };
        feedback.prose_correction = Some(ProseCorrection::UnverifiedConfigurationClaimAt(
            ReplayLocation {
                start: position.clone(),
                end: position,
            },
        ));
        let cap = feedback_bytes(policy)?;
        let replay = bounded_bytes(&feedback, cap).map_or(cap, |bytes| bytes.len());
        Ok(TypedFeedbackSizeEnvelope("\\".repeat(replay.max(
            acquisition_choices::feedback_encoded_bound(contract, ledger, policy)?,
        ))))
    }

    /// Schema-six structural feedback is bounded by the existing encoded-byte
    /// cap B. A string containing ceil(B/2) backslashes serializes to at least
    /// B+2 bytes, all quotes/backslashes. At each further string embedding every
    /// one of those bytes doubles; arbitrary already-serialized JSON can grow
    /// by at most that factor (plus the same two framing quotes). This proves
    /// domination at every embedding layer, not merely in the direct mapping.
    ///
    /// Combined feedback has only fixed codes, integer positions and validated
    /// evidence/obligation IDs, so redact-text@2 cannot expand its strings.
    /// Citation guidance may contain arbitrary identities: retain its existing
    /// conservative envelope rather than assuming those strings are inert.
    /// This is sizing-only and does not activate the new finish family.
    pub fn sizing_combined_contract_envelope(
        contract: &InvestigationCompletionContract,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<TypedFeedbackSizeEnvelope, InvestigationError> {
        let structural = feedback_bytes(policy)?.div_ceil(2);
        let citation = acquisition_choices::feedback_encoded_bound(contract, ledger, policy)?;
        Ok(TypedFeedbackSizeEnvelope(
            "\\".repeat(structural.max(citation)),
        ))
    }

    /// Derive a maximal feedback representation for this exact normalized
    /// obligation set and the immutable policy resolved from saved task limits.
    ///
    /// Every canonical obligation contributes its actual id and kind. The
    /// longest current serialized reason is `acquired_not_supplied`; no real
    /// read or finish decision is asserted. Ids, kinds and current reason codes
    /// contain no JSON-escaped characters: the longest reason dominates at each
    /// embedding layer, and adding every entry dominates every valid subset.
    /// This assumes the other enclosing fields are kept unchanged.
    /// Callers must serialize this sizing
    /// component through their actual enclosing structures and include every
    /// other prompt component before claiming any larger bound.
    pub fn sizing_upper_envelope_with_policy(
        obligations: &ObligationEnvelope,
        policy: &InvestigationPolicy,
    ) -> Result<RefusalFeedbackSizeEnvelope, InvestigationError> {
        if obligations.normalized_with_policy(policy)? != *obligations {
            return Err(InvestigationError::InvalidInput);
        }
        let feedback = Self {
            prose_correction: None,
            schema_version: 1,
            citation_correction: None,
            unmet: obligations
                .obligations
                .iter()
                .map(|obligation| {
                    let (kind, id) = obligation.key();
                    FeedbackEntry {
                        obligation_id: id.into(),
                        kind: kind.into(),
                        reason: RefusalReason::AcquiredNotSupplied,
                    }
                })
                .collect(),
        };
        feedback.bytes_with_policy(policy)?;
        Ok(RefusalFeedbackSizeEnvelope { feedback })
    }

    pub fn bytes(&self) -> Result<Vec<u8>, InvestigationError> {
        self.bytes_with_policy(&INVESTIGATION_V1_POLICY)
    }
    /// Exact stored-feedback ceiling, so the host can preflight a saved record
    /// before decoding it. v1 remains 2048 bytes.
    pub fn max_bytes_with_policy(
        policy: &InvestigationPolicy,
    ) -> Result<usize, InvestigationError> {
        feedback_bytes(policy)
    }
    /// The host resolves `policy` from the exact saved task limits.
    pub fn bytes_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<Vec<u8>, InvestigationError> {
        if self.schema_version
            != if matches!(self.prose_correction, Some(ProseCorrection::Combined(_))) {
                6
            } else if matches!(
                self.prose_correction,
                Some(
                    ProseCorrection::UnverifiedEnumerationClaimAt(_)
                        | ProseCorrection::UnverifiedConfigurationClaimAt(_)
                )
            ) {
                5
            } else if matches!(
                self.prose_correction,
                Some(ProseCorrection::ParaphraseReplayAt(_))
            ) {
                4
            } else if self.prose_correction.is_some() {
                3
            } else if self.citation_correction.is_some() {
                2
            } else {
                1
            }
            || (self.unmet.is_empty() && self.prose_correction.is_none())
            || (self.prose_correction.is_some() && self.citation_correction.is_some())
            || self.unmet.len() > policy.max_completion_obligations()
            || self.unmet.iter().any(|entry| {
                !crate::source_basis::hash(&entry.obligation_id)
                    || !matches!(entry.kind.as_str(), "source" | "inventory")
            })
            || self.unmet.windows(2).any(|entries| {
                (&entries[0].kind, &entries[0].obligation_id)
                    >= (&entries[1].kind, &entries[1].obligation_id)
            })
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(ProseCorrection::Combined(combined)) = &self.prose_correction {
            combined.validate(policy)?;
        }
        if let Some(ProseCorrection::LimitationDeniesAcquiredEvidence(conflict)) =
            &self.prose_correction
            && (conflict.evidence_ids.is_empty()
                || conflict.evidence_ids.len() > policy.limits().evidence_items
                || !conflict
                    .evidence_ids
                    .windows(2)
                    .all(|pair| pair[0] < pair[1])
                || !conflict.evidence_ids.iter().all(|id| {
                    id.strip_prefix("evidence-").is_some_and(|digits| {
                        !digits.starts_with('0')
                            && digits.bytes().all(|b| b.is_ascii_digit())
                            && digits
                                .parse::<usize>()
                                .is_ok_and(|n| n <= policy.limits().evidence_items)
                    })
                }))
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(
            ProseCorrection::ParaphraseReplayAt(location)
            | ProseCorrection::UnverifiedEnumerationClaimAt(location)
            | ProseCorrection::UnverifiedConfigurationClaimAt(location),
        ) = &self.prose_correction
        {
            location.validate()?;
        }
        if let Some(correction) = &self.citation_correction {
            correction.validate_feedback(&self.unmet, policy)?;
        }
        // The correction shares the old total ceiling with generic feedback.
        // Component validity never implies that their combination fits.
        bounded_bytes(self, feedback_bytes(policy)?)
    }
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        self.fingerprint_with_policy(&INVESTIGATION_V1_POLICY)
    }
    pub fn fingerprint_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<String, InvestigationError> {
        Ok(crate::source_basis::domain_hash(
            b"cartograph:obligation-feedback:v1\0",
            &self.bytes_with_policy(policy)?,
        ))
    }
}

#[cfg(test)]
#[path = "feedback_capacity_tests.rs"]
mod feedback_capacity_tests;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationObservation {
    pub obligation_id: String,
    pub kind: String,
    pub attempted: bool,
    pub acquired: bool,
    pub exactly_supplied: bool,
    pub unmet: Option<RefusalReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_outcome: Option<CompletionAttemptOutcome>,
}
pub struct AcceptedCandidate {
    pub outcome: CompletionOutcome,
    pub findings: Vec<InvestigationFinding>,
    pub knowledge_completeness: KnowledgeCompleteness,
    pub limitations: Vec<String>,
    pub assessment: Vec<ObligationObservation>,
}
pub enum FinishDecision {
    Accepted(AcceptedCandidate),
    Refused {
        assessment: Vec<ObligationObservation>,
        feedback: RefusalFeedback,
    },
}

#[allow(clippy::too_many_arguments)]
pub fn admit_finish(
    request: &StagedObligationRequest,
    task: &str,
    step: &str,
    input: &InvestigationInput,
    disclosure: &InvestigationDisclosure,
    finish: &StagedFinish,
    attempts: &[AttemptEvidence<'_>],
    model: Option<String>,
    timestamp: String,
) -> Result<FinishDecision, InvestigationError> {
    admit_finish_with_policy(
        request,
        task,
        step,
        input,
        disclosure,
        finish,
        attempts,
        model,
        timestamp,
        &INVESTIGATION_V1_POLICY,
    )
}

/// As [`admit_finish`], bounded by the immutable policy the host resolved
/// from the exact saved task limits; it must name the request's profile.
#[allow(clippy::too_many_arguments)]
pub fn admit_finish_with_policy(
    request: &StagedObligationRequest,
    task: &str,
    step: &str,
    input: &InvestigationInput,
    disclosure: &InvestigationDisclosure,
    finish: &StagedFinish,
    attempts: &[AttemptEvidence<'_>],
    model: Option<String>,
    timestamp: String,
    policy: &InvestigationPolicy,
) -> Result<FinishDecision, InvestigationError> {
    admit_finish_with_policy_using_consistency(
        request, task, step, input, disclosure, finish, attempts, model, timestamp, policy, None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn admit_finish_with_policy_using_consistency(
    request: &StagedObligationRequest,
    task: &str,
    step: &str,
    input: &InvestigationInput,
    disclosure: &InvestigationDisclosure,
    finish: &StagedFinish,
    attempts: &[AttemptEvidence<'_>],
    model: Option<String>,
    timestamp: String,
    policy: &InvestigationPolicy,
    consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
) -> Result<FinishDecision, InvestigationError> {
    admit_finish_with_locator(
        request,
        task,
        step,
        input,
        disclosure,
        finish,
        attempts,
        model,
        timestamp,
        policy,
        consistency,
        InvestigationInput::correction_replay_location,
    )
}

// The injected diagnostic has no authority to admit a finish; the unchanged
// admission and prose guards below establish rejection before it is called.
#[allow(clippy::too_many_arguments)]
pub(super) fn admit_finish_with_locator(
    request: &StagedObligationRequest,
    task: &str,
    step: &str,
    input: &InvestigationInput,
    disclosure: &InvestigationDisclosure,
    finish: &StagedFinish,
    attempts: &[AttemptEvidence<'_>],
    model: Option<String>,
    timestamp: String,
    policy: &InvestigationPolicy,
    consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
    locator: impl FnOnce(
        &InvestigationInput,
        &StagedFinish,
        Option<&str>,
    ) -> Result<Option<ReplayLocation>, InvestigationError>,
) -> Result<FinishDecision, InvestigationError> {
    finish.validate_for_role(matches!(
        request.specialist_id.as_str(),
        "spec-writer@2" | "gap-triager@2"
    ))?;
    let request = request.normalized_with_policy(policy)?;
    if request.limit_profile != policy.profile() {
        return Err(InvestigationError::InvalidInput);
    }
    if matches!(
        request.specialist_id.as_str(),
        "spec-writer@1" | "spec-writer@2" | "gap-triager@1" | "gap-triager@2"
    ) && matches!(
        &finish.action,
        InvestigationAction::Finish { findings, .. }
            if findings.iter().any(|finding| finding.claim_kind != InvestigationClaimKind::ProposedDesign)
    ) {
        return Err(InvestigationError::InvalidAction);
    }
    // A section draft describes the system as it is (SPEC-15 slice 6).
    if request.specialist_id == "section-writer@1"
        && matches!(
            &finish.action,
            InvestigationAction::Finish { findings, .. }
                if findings.iter().any(|finding| finding.claim_kind == InvestigationClaimKind::ProposedDesign)
        )
    {
        return Err(InvestigationError::InvalidAction);
    }
    let ledger = input.ledger();
    if matches!(disclosure.schema_version, 4..=6) {
        input.validate_completion_binding_with_policy(&request, disclosure, attempts, policy)?;
    }
    if request
        .expected_graph_revision
        .as_deref()
        .is_some_and(|graph| graph != ledger.graph_snapshot_id)
        || ledger
            .coverage
            .as_ref()
            .is_none_or(|proof| proof.context_id != request.expected_context_id)
        || attempts.len() > policy.max_completed_attempts()
    {
        return Err(InvestigationError::InvalidInput);
    }
    for item in &request.obligations.obligations {
        if let Obligation::Inventory { target, .. } = item {
            let proof = ledger
                .coverage
                .as_ref()
                .ok_or(InvestigationError::InvalidInput)?;
            if !proof.qualification.repos.iter().any(|repo| {
                repo.repo == target.repo
                    && repo.status == context_hub::coverage::CoverageStatus::Recorded
                    && repo.inventory_hash.as_deref() == Some(target.inventory_hash.as_str())
            }) {
                return Err(InvestigationError::InvalidInput);
            }
        }
    }
    input.validate_response_metadata(model.as_deref())?;
    let mut ids = BTreeSet::new();
    for evidence in attempts {
        evidence
            .record
            .validate_with_policy(&request, task, evidence.ledger, policy)?;
        if !ids.insert(&evidence.record.attempt_id) {
            return Err(InvestigationError::InvalidInput);
        }
    }
    for evidence in attempts {
        if let AttemptOutcome::FirewallRefused {
            acquired_attempt_id,
            ..
        } = &evidence.record.outcome
            && !attempts.iter().any(|prior| {
                prior.record.attempt_id == *acquired_attempt_id
                    && prior.record.obligation_id == evidence.record.obligation_id
                    && matches!(
                        prior.record.outcome,
                        AttemptOutcome::SourceAcquired { .. }
                            | AttemptOutcome::InventoryAcquired { .. }
                    )
            })
        {
            return Err(InvestigationError::InvalidInput);
        }
    }
    let assessment = derive_assessment_for_binding_with_policy_using_consistency(
        &request.intent_hash_with_policy(policy)?,
        &request.obligations,
        task,
        step,
        ledger,
        disclosure,
        finish.outcome,
        attempts,
        policy,
        consistency,
    )?;
    let completion = if matches!(disclosure.schema_version, 4..=6) {
        Some(InvestigationCompletionResult {
            withholding: None,
            schema_version: 1,
            outcome: finish.outcome,
            request_hash: request.intent_hash_with_policy(policy)?,
            obligations_hash: request.obligations.fingerprint_with_policy(policy)?,
            finishing_attempts_hash: completed_attempts_hash_with_policy(attempts, policy)?,
            assessment: assessment.clone(),
        })
    } else {
        None
    };
    // All security/citation/prose replay admission precedes a recoverable decision.
    let admitted = input.admit_result_bound_with_policy(
        task,
        step,
        Some(disclosure),
        &finish.action,
        model.clone(),
        timestamp,
        completion,
        policy,
        consistency,
    );
    let mut citation_correction = None;
    let mut prose_correction = None;
    let admitted = match admitted {
        Ok(result) => match input.admit_narrative(result, finish) {
            Err(InvestigationError::SourceReplay) => Err(InvestigationError::SourceReplay),
            other => Ok(other?),
        },
        error => error,
    };
    let admitted = match admitted {
        Ok(result) => Some(result),
        Err(InvestigationError::SourceReplay)
            if matches!(
                request.specialist_id.as_str(),
                "domain-analyst@24"
                    | "domain-analyst@25"
                    | "domain-analyst@26"
                    | "domain-analyst@27"
                    | "domain-analyst@28"
                    | "domain-analyst@29"
            ) && consistency.is_some() =>
        {
            match input.validate_correction_prose(finish, model.as_deref()) {
                Err(InvestigationError::SourceReplay) => (),
                Err(error) => return Err(error),
                Ok(()) => return Err(InvestigationError::SourceReplay),
            }
            // Collect every detector family below; do not select a first-only
            // legacy correction or suppress claims because replay was found.
            None
        }
        Err(InvestigationError::SourceReplay)
            if matches!(
                request.specialist_id.as_str(),
                "domain-analyst@20"
                    | "domain-analyst@21"
                    | "domain-analyst@22"
                    | "evidence-auditor@13"
                    | "evidence-auditor@14"
                    | "domain-analyst@23"
            ) && consistency.is_some() =>
        {
            // Check all authored fields, including narrative, for sensitive output.
            match input.validate_correction_prose(finish, model.as_deref()) {
                Err(InvestigationError::SourceReplay) => (),
                Err(error) => return Err(error),
                Ok(()) => return Err(InvestigationError::SourceReplay),
            }
            prose_correction = Some(
                if matches!(
                    request.specialist_id.as_str(),
                    "domain-analyst@22"
                        | "evidence-auditor@13"
                        | "evidence-auditor@14"
                        | "domain-analyst@23"
                ) {
                    match locator(input, finish, model.as_deref()) {
                        Ok(Some(location))
                            if matches!(
                                location.start.field,
                                replay_location::ProseField::Model
                            ) || matches!(
                                location.end.field,
                                replay_location::ProseField::Model
                            ) =>
                        {
                            // Provider metadata cannot be rewritten by the analyst.
                            ProseCorrection::ParaphraseWithoutQuotingSource
                        }
                        Ok(Some(location)) => ProseCorrection::ParaphraseReplayAt(location),
                        // A diagnostic discrepancy must not consume the correction
                        // turn. The finish remains refused by the guard above.
                        Ok(None) | Err(InvestigationError::SourceReplay) => {
                            ProseCorrection::ParaphraseWithoutQuotingSource
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    ProseCorrection::ParaphraseWithoutQuotingSource
                },
            );
            None
        }
        Err(InvestigationError::InvalidCitation)
            if request.specialist_id == "domain-analyst@19"
                && input.eligible_citation_correction(
                    disclosure,
                    finish,
                    model.as_deref(),
                    policy,
                    consistency,
                )? =>
        {
            None
        }
        Err(InvestigationError::InvalidCitation)
            if matches!(
                request.specialist_id.as_str(),
                "domain-analyst@20"
                    | "domain-analyst@21"
                    | "domain-analyst@22"
                    | "evidence-auditor@13"
                    | "evidence-auditor@14"
                    | "domain-analyst@23"
                    | "domain-analyst@24"
                    | "domain-analyst@25"
                    | "domain-analyst@26"
                    | "domain-analyst@27"
                    | "domain-analyst@28"
                    | "domain-analyst@29"
            ) =>
        {
            citation_correction = input.typed_citation_correction(
                disclosure,
                finish,
                model.as_deref(),
                policy,
                consistency,
            )?;
            if citation_correction.is_none() {
                return Err(InvestigationError::InvalidCitation);
            }
            None
        }
        Err(error) => return Err(error),
    };
    let unmet = assessment
        .iter()
        .filter_map(|observation| {
            observation.unmet.map(|reason| FeedbackEntry {
                obligation_id: observation.obligation_id.clone(),
                kind: observation.kind.clone(),
                reason: if matches!(
                    request.specialist_id.as_str(),
                    "evidence-auditor@14"
                        | "domain-analyst@22"
                        | "domain-analyst@23"
                        | "domain-analyst@24"
                        | "domain-analyst@25"
                        | "domain-analyst@26"
                        | "domain-analyst@27"
                        | "domain-analyst@28"
                        | "domain-analyst@29"
                ) && !observation.attempted
                    && !observation.acquired
                    && reason == RefusalReason::NotAttempted
                    && consistency.is_some_and(|bound| {
                        bound
                            .capacity_excluded_obligations()
                            .contains(&observation.obligation_id)
                    }) {
                    RefusalReason::ExcludedByCapacity
                } else {
                    reason
                },
            })
        })
        .collect::<Vec<_>>();
    if matches!(
        request.specialist_id.as_str(),
        "domain-analyst@20" | "domain-analyst@21" | "domain-analyst@22" | "domain-analyst@23"
    ) && admitted.is_some()
        && prose::has_placeholder(finish)
    {
        prose_correction = Some(ProseCorrection::RewriteSchemaPlaceholdersAsPlainProse);
    }
    if matches!(
        request.specialist_id.as_str(),
        "domain-analyst@20" | "domain-analyst@21" | "domain-analyst@22" | "domain-analyst@23"
    ) && consistency.is_some()
        && prose_correction.is_none()
        && let Some(result) = &admitted
    {
        let overall = result
            .limitations
            .iter()
            .map(|text| (text.as_str(), &[][..]));
        let local = result.findings.iter().flat_map(|finding| {
            finding
                .limitations
                .iter()
                .map(|text| (text.as_str(), finding.citation_ids.as_slice()))
        });
        let limitations = overall.chain(local);
        prose_correction = limitation_check::correction(ledger, limitations);
    }
    if request.specialist_id == "domain-analyst@23"
        && admitted.is_some()
        && consistency.is_some()
        && prose_correction.is_none()
    {
        prose_correction = input.claim_correction(finish)?;
    }
    if matches!(
        request.specialist_id.as_str(),
        "domain-analyst@24"
            | "domain-analyst@25"
            | "domain-analyst@26"
            | "domain-analyst@27"
            | "domain-analyst@28"
            | "domain-analyst@29"
    ) && consistency.is_some()
        && citation_correction.is_none()
        && let Some(feedback) = RefusalFeedback::collect_input_corrections_with_policy(
            input,
            finish,
            model.as_deref(),
            unmet.clone(),
            policy,
        )?
    {
        return Ok(FinishDecision::Refused {
            assessment,
            feedback,
        });
    }
    if unmet.is_empty() && prose_correction.is_none() {
        // A correction never constructs a result or discharges an obligation.
        let admitted = admitted.ok_or(InvestigationError::InvalidCitation)?;
        Ok(FinishDecision::Accepted(AcceptedCandidate {
            outcome: finish.outcome,
            findings: admitted.findings,
            knowledge_completeness: admitted.knowledge_completeness,
            limitations: admitted.limitations,
            assessment,
        }))
    } else {
        let feedback = RefusalFeedback {
            schema_version: if matches!(
                prose_correction,
                Some(
                    ProseCorrection::UnverifiedEnumerationClaimAt(_)
                        | ProseCorrection::UnverifiedConfigurationClaimAt(_)
                )
            ) {
                5
            } else if matches!(
                prose_correction,
                Some(ProseCorrection::ParaphraseReplayAt(_))
            ) {
                4
            } else if prose_correction.is_some() {
                3
            } else if citation_correction.is_some() {
                2
            } else {
                1
            },
            prose_correction,
            citation_correction,
            unmet,
        };
        feedback.bytes_with_policy(policy)?;
        Ok(FinishDecision::Refused {
            assessment,
            feedback,
        })
    }
}

/// Re-derivable metadata predicate. Security admission remains mandatory before
/// a refusal is first stored; historical reads never reconstruct withheld prose.
#[allow(clippy::too_many_arguments)]
pub fn derive_assessment(
    request: &StagedObligationRequest,
    task: &str,
    step: &str,
    ledger: &InvestigationInputLedger,
    disclosure: &InvestigationDisclosure,
    outcome: CompletionOutcome,
    attempts: &[AttemptEvidence<'_>],
) -> Result<Vec<ObligationObservation>, InvestigationError> {
    let request = request.normalized()?;
    derive_assessment_for_binding(
        &request.intent_hash()?,
        &request.obligations,
        task,
        step,
        ledger,
        disclosure,
        outcome,
        attempts,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn derive_assessment_for_binding(
    request_hash: &str,
    envelope: &ObligationEnvelope,
    task: &str,
    step: &str,
    ledger: &InvestigationInputLedger,
    disclosure: &InvestigationDisclosure,
    outcome: CompletionOutcome,
    attempts: &[AttemptEvidence<'_>],
) -> Result<Vec<ObligationObservation>, InvestigationError> {
    derive_assessment_for_binding_with_policy(
        request_hash,
        envelope,
        task,
        step,
        ledger,
        disclosure,
        outcome,
        attempts,
        &INVESTIGATION_V1_POLICY,
    )
}
/// As [`derive_assessment_for_binding`], bounded by the saved task policy.
#[allow(clippy::too_many_arguments)]
pub fn derive_assessment_for_binding_with_policy(
    request_hash: &str,
    envelope: &ObligationEnvelope,
    task: &str,
    step: &str,
    ledger: &InvestigationInputLedger,
    disclosure: &InvestigationDisclosure,
    outcome: CompletionOutcome,
    attempts: &[AttemptEvidence<'_>],
    policy: &InvestigationPolicy,
) -> Result<Vec<ObligationObservation>, InvestigationError> {
    derive_assessment_for_binding_with_policy_using_consistency(
        request_hash,
        envelope,
        task,
        step,
        ledger,
        disclosure,
        outcome,
        attempts,
        policy,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn derive_assessment_for_binding_with_policy_using_consistency(
    request_hash: &str,
    envelope: &ObligationEnvelope,
    task: &str,
    step: &str,
    ledger: &InvestigationInputLedger,
    disclosure: &InvestigationDisclosure,
    outcome: CompletionOutcome,
    attempts: &[AttemptEvidence<'_>],
    policy: &InvestigationPolicy,
    consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
) -> Result<Vec<ObligationObservation>, InvestigationError> {
    if let Some(bound) = consistency {
        bound.check(task, step, ledger, disclosure)?;
    }
    let envelope = envelope.normalized_with_policy(policy)?;
    disclosure.validate_consistent(ledger, policy, consistency)?;
    if attempts.len() > policy.max_completed_attempts() {
        return Err(InvestigationError::LimitExceeded);
    }
    for evidence in attempts {
        evidence.record.validate_binding_with_policy(
            request_hash,
            &envelope,
            task,
            evidence.ledger,
            policy,
        )?;
    }
    let sources = envelope.sources_with_policy(policy)?;
    let source_assessment = if sources.is_empty() {
        None
    } else {
        Some(
            InvestigationInspectionAssessment::derive_with_policy_using_consistency(
                &sources,
                ledger,
                step,
                disclosure,
                &[],
                policy,
                consistency,
            )?,
        )
    };
    let mut assessment = Vec::new();
    for obligation in &envelope.obligations {
        let (kind, id) = obligation.key();
        let matching: Vec<_> = attempts
            .iter()
            .filter(|evidence| evidence.record.obligation_id == id)
            .collect();
        let attempted = !matching.is_empty();
        let observed_acquired = matching.iter().any(|evidence| {
            matches!(
                evidence.record.outcome,
                AttemptOutcome::SourceAcquired { .. } | AttemptOutcome::InventoryAcquired { .. }
            )
        });
        let (acquired, supplied, redacted, mismatched) = match obligation {
            Obligation::Source { target, .. } => {
                let observation = source_assessment
                    .as_ref()
                    .and_then(|assessment| {
                        assessment
                            .requirements
                            .iter()
                            .find(|item| item.target_id == target.target_id)
                    })
                    .ok_or(InvestigationError::InvalidInput)?;
                let actual: Vec<_> = ledger
                    .citations
                    .iter()
                    .filter(|citation| target.matches(citation))
                    .flat_map(|citation| {
                        disclosure.sources.iter().filter(move |source| {
                            source.citation_id == citation.citation_id
                                && source.representation.is_supplied()
                        })
                    })
                    .collect();
                (
                    observation.acquired,
                    observation.supplied,
                    actual.iter().any(|source| source.redacted),
                    actual.iter().any(|source| !source.redacted),
                )
            }
            Obligation::Inventory { target, .. } => {
                let proof = ledger
                    .coverage
                    .as_ref()
                    .ok_or(InvestigationError::InvalidInput)?;
                let indices: BTreeSet<_> = ledger
                    .coverage_reads
                    .iter()
                    .enumerate()
                    .filter_map(|(index, read)| {
                        (read.validate(proof).is_ok()
                            && read.request.context_id == target.context_id
                            && read.request.repo == target.repo
                            && read.request.inventory_hash == target.inventory_hash)
                            .then_some(index)
                    })
                    .collect();
                let actual: Vec<_> = disclosure
                    .coverage_details
                    .iter()
                    .filter(|detail| indices.contains(&detail.read_index))
                    .collect();
                (
                    !indices.is_empty(),
                    actual.iter().any(|detail| {
                        !detail.redacted && detail.disclosed_body_hash == target.inventory_hash
                    }),
                    actual.iter().any(|detail| detail.redacted),
                    actual.iter().any(|detail| {
                        !detail.redacted && detail.disclosed_body_hash != target.inventory_hash
                    }),
                )
            }
        };
        // Acquired observations must actually exist in the finishing ledger;
        // attempted is established only by completed durable records, never flags.
        if observed_acquired && !acquired {
            return Err(InvestigationError::InvalidInput);
        }
        let reason = if outcome == CompletionOutcome::InsufficientEvidence {
            (!attempted).then_some(RefusalReason::NotAttempted)
        } else if supplied {
            None
        } else if !attempted {
            Some(RefusalReason::NotAttempted)
        } else if !acquired {
            Some(RefusalReason::Unavailable)
        } else if redacted {
            Some(RefusalReason::Redacted)
        } else if mismatched {
            Some(RefusalReason::HashMismatch)
        } else {
            Some(RefusalReason::AcquiredNotSupplied)
        };
        assessment.push(ObligationObservation {
            obligation_id: id.into(),
            kind: kind.into(),
            attempted,
            acquired,
            exactly_supplied: supplied,
            unmet: reason,
            last_attempt_outcome: matching
                .last()
                .map(|evidence| match evidence.record.outcome {
                    AttemptOutcome::SourceAcquired { .. }
                    | AttemptOutcome::InventoryAcquired { .. } => {
                        CompletionAttemptOutcome::Acquired
                    }
                    AttemptOutcome::Unavailable { .. } => CompletionAttemptOutcome::Unavailable,
                    AttemptOutcome::FirewallRefused { .. } => {
                        CompletionAttemptOutcome::FirewallRefused
                    }
                }),
        });
    }
    Ok(assessment)
}

// Nonsemantic nested stages describe a host-reserialized action, not the raw
// response. Retain the historical coarse label rather than imply otherwise.
fn nested_finish_failure(failure: ActionDecodeFailure) -> ActionDecodeFailure {
    match failure.stage {
        ActionDecodeStage::SemanticRule(_) => failure,
        _ => ActionSemanticRule::FinishShape.failure(failure.error),
    }
}

#[cfg(test)]
#[test]
fn nested_finish_failure_mapping_preserves_errors_without_claiming_reachability() {
    use ActionDecodeStage as Stage;
    for error in [
        InvestigationError::InvalidAction,
        InvestigationError::InvalidInput,
        InvestigationError::LimitExceeded,
    ] {
        for stage in [
            Stage::Preflight,
            Stage::TypedEnvelope,
            Stage::TypedStagedFinish,
            Stage::TypedCoverageRead,
            Stage::TypedLegacyAction,
            Stage::TypedReplay,
            Stage::BareFinish,
            Stage::CanonicalFields,
        ] {
            assert_eq!(
                nested_finish_failure(ActionDecodeFailure { stage, error }),
                ActionSemanticRule::FinishShape.failure(error)
            );
        }
        let semantic = ActionSemanticRule::FindingCitations.failure(error);
        assert_eq!(nested_finish_failure(semantic), semantic);
    }
}

#[path = "finish_prose.rs"]
mod prose;

#[path = "limitation_check.rs"]
mod limitation_check;
