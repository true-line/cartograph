//! Versioned completion projections. Execution status is a separate state axis.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::{
    obligations::ObligationEnvelope,
    staged_finish::{CompletionOutcome, ObligationObservation},
    *,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionStatus {
    NotAssessed,
    Satisfied,
    Unmet,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationCompletionOutcome {
    Supported,
    FindingsWithheldAfterCorrection,
    InsufficientEvidence,
    ObligationsUnmet,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionAttemptOutcome {
    Acquired,
    Unavailable,
    FirewallRefused,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionRefusalBasis {
    pub step_id: String,
    pub input_ledger_hash: String,
    pub disclosure_hash: String,
    pub finish_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompletionAssessmentBasis {
    NotAssessed,
    FinishingDisclosure {
        step_id: String,
        input_ledger_hash: String,
        disclosure_hash: String,
        finishing_attempts_hash: String,
    },
    TerminalObservation {
        finishing_attempts_hash: String,
        last_refusal_id: Option<String>,
        last_refusal_basis: Option<CompletionRefusalBasis>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCompletionDetail {
    pub schema_version: u32,
    pub request_hash: String,
    pub obligations_hash: String,
    pub obligations: ObligationEnvelope,
    pub refusal_count: u32,
    pub assessment_basis: CompletionAssessmentBasis,
    pub assessment: Vec<ObligationObservation>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCompletionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withholding: Option<FindingWithholdingWitness>,
    pub schema_version: u32,
    pub outcome: CompletionOutcome,
    pub request_hash: String,
    pub obligations_hash: String,
    pub finishing_attempts_hash: String,
    pub assessment: Vec<ObligationObservation>,
}
/// Mandatory immutable input6 object, distinct from the normalized envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCompletionContract {
    pub schema_version: u32,
    pub request_hash: String,
    pub obligations_hash: String,
    pub obligations: ObligationEnvelope,
}
impl InvestigationCompletionContract {
    pub fn validate(&self) -> Result<(), InvestigationError> {
        self.validate_with_policy(&INVESTIGATION_V1_POLICY)
    }

    pub fn validate_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        if self.schema_version != 1
            || !crate::source_basis::hash(&self.request_hash)
            || self.obligations != self.obligations.normalized_with_policy(policy)?
            || self.obligations.fingerprint_with_policy(policy)? != self.obligations_hash
        {
            return Err(InvestigationError::InvalidInput);
        }
        bounded_bytes(self, MAX_INPUT_BYTES)?;
        Ok(())
    }
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        self.fingerprint_with_policy(&INVESTIGATION_V1_POLICY)
    }

    pub fn fingerprint_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<String, InvestigationError> {
        self.validate_with_policy(policy)?;
        Ok(crate::source_basis::domain_hash(
            b"cartograph:completion-contract:v1\0",
            &bounded_bytes(self, MAX_INPUT_BYTES)?,
        ))
    }
}

/// The IPC object stays flat; generation is inspected before decoding one arm.
#[derive(Debug, Clone)]
pub enum InvestigationStartRequest {
    Legacy(StartInvestigationRequest),
    Completion(super::obligations::StagedObligationRequest),
}
impl InvestigationStartRequest {
    pub fn decode(raw: &str) -> Result<Self, InvestigationError> {
        strict::preflight(
            raw,
            InvestigationPolicy::maximum_request_bytes(),
            32,
            4096,
            128,
        )?;
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidRequest)?;
        match value["schema_version"].as_u64() {
            Some(1..=3) => {
                strict::preflight(raw, INVESTIGATION_V1_POLICY.request_bytes(), 32, 4096, 128)?;
                let request: StartInvestigationRequest =
                    serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidRequest)?;
                request.validate()?;
                Ok(Self::Legacy(request))
            }
            Some(4) => Ok(Self::Completion(
                super::obligations::StagedObligationRequest::decode(raw)?,
            )),
            Some(5..=7) => {
                let policy = value["limit_profile"]
                    .as_str()
                    .and_then(InvestigationPolicy::for_profile)
                    .filter(|policy| policy.profile() == "investigation-v2")
                    .ok_or(InvestigationError::InvalidRequest)?;
                Ok(Self::Completion(
                    super::obligations::StagedObligationRequest::decode_with_policy(raw, policy)?,
                ))
            }
            _ => Err(InvestigationError::InvalidRequest),
        }
    }
}
impl Serialize for InvestigationStartRequest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Legacy(request) => request.serialize(serializer),
            Self::Completion(request) => request.serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for InvestigationStartRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // IPC may already have parsed arguments into Value, collapsing duplicate
        // keys. Only discriminate the generation and decode its closed shape here:
        // semantic failures must reach start's structured invalid_input response.
        // Raw-protocol callers use decode() for strict preflight and validation.
        let raw = Box::<serde_json::value::RawValue>::deserialize(deserializer)?;
        let value: serde_json::Value =
            serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?;
        match value["schema_version"].as_u64() {
            Some(1..=3) => serde_json::from_str(raw.get())
                .map(Self::Legacy)
                .map_err(serde::de::Error::custom),
            Some(4..=7) => serde_json::from_str(raw.get())
                .map(Self::Completion)
                .map_err(serde::de::Error::custom),
            _ => Err(serde::de::Error::custom(
                "Unsupported investigation request generation",
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub enum CompletionInvestigationAction {
    Read(QualifiedInvestigationAction),
    Finish(super::staged_finish::StagedFinish),
}
impl CompletionInvestigationAction {
    pub fn decode_message(raw: &str, context_id: &str) -> Result<Self, InvestigationError> {
        strict::preflight(raw, MAX_ACTION_BYTES, 32, 4096, 32)?;
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidAction)?;
        if value.get("schema_version").is_some() {
            return super::staged_finish::StagedFinish::decode_message(raw).map(Self::Finish);
        }
        Self::decode(raw, context_id)
    }
    pub fn decode(raw: &str, context_id: &str) -> Result<Self, InvestigationError> {
        Self::decode_detailed(raw, context_id).map_err(|failure| failure.error)
    }
    pub(super) fn decode_detailed(
        raw: &str,
        context_id: &str,
    ) -> Result<Self, ActionDecodeFailure> {
        use ActionDecodeStage as Stage;
        strict::preflight(raw, MAX_ACTION_BYTES, 32, 4096, 32)
            .map_err(|error| Stage::Preflight.failure(error))?;
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| Stage::TypedEnvelope.invalid())?;
        if value.get("schema_version").is_some() {
            return super::staged_finish::StagedFinish::decode_detailed(raw).map(Self::Finish);
        }
        match QualifiedInvestigationAction::decode_detailed(raw, context_id)? {
            QualifiedInvestigationAction::SourceOrFinish(InvestigationAction::Finish {
                ..
            }) => Err(Stage::BareFinish.invalid()),
            read => Ok(Self::Read(read)),
        }
    }
}

/// Ordered, duplicate-free completed observations, including unavailable reads.
pub fn completed_attempts_hash(
    attempts: &[super::staged_attempt::AttemptEvidence<'_>],
) -> Result<String, InvestigationError> {
    completed_attempts_hash_with_policy(attempts, &INVESTIGATION_V1_POLICY)
}

/// Policy-aware journal identity. The host must resolve the immutable policy
/// from the exact saved task limits; this does not authorize new request routes.
pub fn completed_attempts_hash_with_policy(
    attempts: &[super::staged_attempt::AttemptEvidence<'_>],
    policy: &InvestigationPolicy,
) -> Result<String, InvestigationError> {
    if attempts.len() > policy.max_completed_attempts() {
        return Err(InvestigationError::InvalidInput);
    }
    let ids: Vec<_> = attempts
        .iter()
        .map(|attempt| attempt.record.attempt_id.as_str())
        .collect();
    completed_attempt_ids_hash_with_policy(&ids, policy)
}

/// Shared ordered ID admission for durable host observations and agent evidence.
/// Hashing IDs does not certify the underlying attempt witnesses.
pub fn completed_attempt_ids_hash_with_policy(
    ids: &[&str],
    policy: &InvestigationPolicy,
) -> Result<String, InvestigationError> {
    if ids.len() > policy.max_completed_attempts()
        || ids.iter().any(|id| !crate::source_basis::hash(id))
        || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
    {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(crate::source_basis::domain_hash(
        b"cartograph:completion-attempts:v1\0",
        &bounded_bytes(&ids, MAX_RECORD_BYTES)?,
    ))
}
impl InvestigationCompletionResult {
    pub fn validate(
        &self,
        findings: &[InvestigationFinding],
        knowledge: KnowledgeCompleteness,
        limitations: &[String],
    ) -> Result<(), InvestigationError> {
        self.validate_with_policy(findings, knowledge, limitations, &INVESTIGATION_V1_POLICY)
    }

    pub fn validate_with_policy(
        &self,
        findings: &[InvestigationFinding],
        knowledge: KnowledgeCompleteness,
        limitations: &[String],
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        if self.schema_version != if self.withholding.is_some() { 2 } else { 1 }
            || [
                &self.request_hash,
                &self.obligations_hash,
                &self.finishing_attempts_hash,
            ]
            .iter()
            .any(|hash| !crate::source_basis::hash(hash))
            || self.assessment.is_empty()
            || self.assessment.len() > policy.max_completion_obligations()
            || self.assessment.windows(2).any(|pair| {
                (&pair[0].kind, &pair[0].obligation_id) >= (&pair[1].kind, &pair[1].obligation_id)
            })
            || self.assessment.iter().any(|observation| {
                !crate::source_basis::hash(&observation.obligation_id)
                    || !matches!(observation.kind.as_str(), "source" | "inventory")
                    || observation.unmet.is_some()
                    || observation.attempted != observation.last_attempt_outcome.is_some()
                    || (observation.exactly_supplied && !observation.acquired)
                    || (observation.capacity_disclosed
                        && (observation.attempted
                            || observation.acquired
                            || observation.exactly_supplied
                            || self.outcome != CompletionOutcome::Supported
                            || self.withholding.is_some()
                            || !limitations.iter().any(|text| {
                                text.contains(super::staged_finish::capacity_name(
                                    &observation.obligation_id,
                                ))
                            })))
            })
            || limitations.is_empty()
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(witness) = &self.withholding {
            witness.validate_shape()?;
            if self.outcome != CompletionOutcome::FindingsWithheldAfterCorrection
                || witness.request_hash != self.request_hash
                || witness.obligations_hash != self.obligations_hash
                || witness.survivor_indexes.len() != findings.len()
            {
                return Err(InvestigationError::InvalidInput);
            }
        } else if self.outcome == CompletionOutcome::FindingsWithheldAfterCorrection {
            return Err(InvestigationError::InvalidInput);
        }
        match self.outcome {
            CompletionOutcome::FindingsWithheldAfterCorrection
                if !findings.is_empty()
                    && knowledge == KnowledgeCompleteness::Partial
                    && self.assessment.iter().all(|item| item.exactly_supplied) =>
            {
                Ok(())
            }
            CompletionOutcome::Supported
                if !findings.is_empty()
                    && knowledge != KnowledgeCompleteness::InsufficientEvidence
                    && self
                        .assessment
                        .iter()
                        .all(|item| item.exactly_supplied || item.capacity_disclosed) =>
            {
                Ok(())
            }
            CompletionOutcome::InsufficientEvidence
                if findings.is_empty()
                    && knowledge == KnowledgeCompleteness::InsufficientEvidence
                    && self.assessment.iter().all(|item| item.attempted) =>
            {
                Ok(())
            }
            _ => Err(InvestigationError::InvalidInput),
        }
    }
}

impl InvestigationCompletionDetail {
    /// Shape and canonical projection checks; the host must independently replay
    /// each observation against the durable invocation/attempt witnesses.
    pub fn validate(&self) -> Result<CompletionStatus, InvestigationError> {
        self.validate_with_policy(&INVESTIGATION_V1_POLICY)
    }

    pub fn validate_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<CompletionStatus, InvestigationError> {
        if self.schema_version != 1
            || !crate::source_basis::hash(&self.request_hash)
            || self.obligations != self.obligations.normalized_with_policy(policy)?
            || self.obligations_hash != self.obligations.fingerprint_with_policy(policy)?
            || self.refusal_count > 2
        {
            return Err(InvestigationError::InvalidInput);
        }
        let hash = |value: &str| crate::source_basis::hash(value);
        let ledger_hash = |value: &str| {
            value
                .strip_prefix("investigation-input-v2:")
                .is_some_and(hash)
                || value
                    .strip_prefix("investigation-input-v1:")
                    .is_some_and(hash)
                || hash(value)
        };
        let disclosure_hash = |value: &str| {
            value
                .strip_prefix("investigation-disclosure-v4:")
                .is_some_and(hash)
                || (policy.profile() == INVESTIGATION_V2_POLICY.profile()
                    && value
                        .strip_prefix("investigation-disclosure-v5:")
                        .is_some_and(hash))
                || (policy.profile() == INVESTIGATION_V2_POLICY.profile()
                    && value
                        .strip_prefix("investigation-disclosure-v6:")
                        .is_some_and(hash))
        };
        let basis_ok = match &self.assessment_basis {
            CompletionAssessmentBasis::NotAssessed => {
                return if self.assessment.is_empty() && self.refusal_count == 0 {
                    Ok(CompletionStatus::NotAssessed)
                } else {
                    Err(InvestigationError::InvalidInput)
                };
            }
            CompletionAssessmentBasis::FinishingDisclosure {
                step_id,
                input_ledger_hash,
                disclosure_hash: disclosed,
                finishing_attempts_hash,
            } => {
                records::opaque_id(step_id)
                    && ledger_hash(input_ledger_hash)
                    && disclosure_hash(disclosed)
                    && hash(finishing_attempts_hash)
            }
            CompletionAssessmentBasis::TerminalObservation {
                finishing_attempts_hash,
                last_refusal_id,
                last_refusal_basis,
            } => {
                hash(finishing_attempts_hash)
                    && last_refusal_id.is_some() == last_refusal_basis.is_some()
                    && (self.refusal_count > 0) == last_refusal_id.is_some()
                    && last_refusal_id.as_deref().is_none_or(hash)
                    && last_refusal_basis.as_ref().is_none_or(|basis| {
                        records::opaque_id(&basis.step_id)
                            && ledger_hash(&basis.input_ledger_hash)
                            && disclosure_hash(&basis.disclosure_hash)
                            && hash(&basis.finish_hash)
                    })
            }
        };
        if !basis_ok
            || self.assessment.len() != self.obligations.obligations.len()
            || self
                .assessment
                .iter()
                .zip(&self.obligations.obligations)
                .any(|(observation, obligation)| {
                    let (kind, id) = obligation.key();
                    observation.kind != kind
                        || observation.obligation_id != id
                        || observation.attempted != observation.last_attempt_outcome.is_some()
                        || (observation.exactly_supplied && !observation.acquired)
                })
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(if self.assessment.iter().all(|item| item.unmet.is_none()) {
            CompletionStatus::Satisfied
        } else {
            CompletionStatus::Unmet
        })
    }
}

impl InvestigationResult {
    /// Reopen validation binds mixed assessment to immutable V7 intent and actual
    /// ordered durable reads. It does not trust persisted fulfillment flags.
    #[allow(clippy::too_many_arguments)]
    pub fn validate_completion(
        &self,
        request: &super::obligations::StagedObligationRequest,
        task: &str,
        step: &str,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        attempts: &[super::staged_attempt::AttemptEvidence<'_>],
    ) -> Result<(), InvestigationError> {
        self.validate_completion_with_policy(
            request,
            task,
            step,
            ledger,
            disclosure,
            attempts,
            &INVESTIGATION_V1_POLICY,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn validate_completion_with_policy(
        &self,
        request: &super::obligations::StagedObligationRequest,
        task: &str,
        step: &str,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        attempts: &[super::staged_attempt::AttemptEvidence<'_>],
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        let request = request.normalized_with_policy(policy)?;
        request.validate_ledger_with_policy(ledger, policy)?;
        let (disclosure_schema, result_schema) = match request.schema_version {
            4 if matches!(
                request.specialist_id.as_str(),
                "spec-writer@2" | "gap-triager@2"
            ) =>
            {
                (4, 8)
            }
            7 => (6, 7),
            6 => (5, 6),
            _ => (4, 5),
        };
        if disclosure.schema_version != disclosure_schema || self.schema_version != result_schema {
            return Err(InvestigationError::InvalidInput);
        }
        self.validate_completion_for_binding_with_policy(
            &request.intent_hash_with_policy(policy)?,
            &request.obligations,
            &request.expected_context_id,
            request.expected_graph_revision.as_deref(),
            task,
            step,
            ledger,
            disclosure,
            attempts,
            policy,
        )
    }

    /// History requires only the immutable saved intent hash and projections,
    /// never retaining or reconstructing the original unredacted question.
    #[allow(clippy::too_many_arguments)]
    pub fn validate_completion_for_binding(
        &self,
        request_hash: &str,
        envelope: &ObligationEnvelope,
        expected_context_id: &str,
        expected_graph_revision: Option<&str>,
        task: &str,
        step: &str,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        attempts: &[super::staged_attempt::AttemptEvidence<'_>],
    ) -> Result<(), InvestigationError> {
        self.validate_completion_for_binding_with_policy(
            request_hash,
            envelope,
            expected_context_id,
            expected_graph_revision,
            task,
            step,
            ledger,
            disclosure,
            attempts,
            &INVESTIGATION_V1_POLICY,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn validate_completion_for_binding_with_policy(
        &self,
        request_hash: &str,
        envelope: &ObligationEnvelope,
        expected_context_id: &str,
        expected_graph_revision: Option<&str>,
        task: &str,
        step: &str,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        attempts: &[super::staged_attempt::AttemptEvidence<'_>],
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        Self::validate_completion_for_binding_with_policy_using_consistency(
            self,
            request_hash,
            envelope,
            expected_context_id,
            expected_graph_revision,
            task,
            step,
            ledger,
            disclosure,
            attempts,
            policy,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn validate_completion_for_binding_with_policy_using_consistency(
        &self,
        request_hash: &str,
        envelope: &ObligationEnvelope,
        expected_context_id: &str,
        expected_graph_revision: Option<&str>,
        task: &str,
        step: &str,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        attempts: &[super::staged_attempt::AttemptEvidence<'_>],
        policy: &InvestigationPolicy,
        consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
    ) -> Result<(), InvestigationError> {
        self.validate_with_disclosure_and_policy_using_consistency(
            ledger,
            step,
            disclosure,
            policy,
            consistency,
        )?;
        let envelope = envelope.normalized_with_policy(policy)?;
        let proof = ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        if proof.context_id != expected_context_id
            || expected_graph_revision.is_some_and(|graph| graph != ledger.graph_snapshot_id)
            || envelope.obligations.iter().any(|item| match item {
                super::obligations::Obligation::Source { target, .. } => {
                    target.graph_snapshot_id != ledger.graph_snapshot_id
                }
                super::obligations::Obligation::Inventory { target, .. } => {
                    target.context_id != expected_context_id
                        || !proof.qualification.repos.iter().any(|repo| {
                            repo.repo == target.repo
                                && repo.status == context_hub::coverage::CoverageStatus::Recorded
                                && repo.inventory_hash.as_deref()
                                    == Some(target.inventory_hash.as_str())
                        })
                }
            })
        {
            return Err(InvestigationError::InvalidInput);
        }
        let completion = self
            .completion
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: request_hash.to_owned(),
            obligations_hash: envelope.fingerprint_with_policy(policy)?,
            obligations: envelope.clone(),
        };
        if self.investigation_id != task
            || disclosure
                .query_history
                .as_ref()
                .is_some_and(|history| history.request_hash != request_hash)
            || completion.request_hash != contract.request_hash
            || completion.obligations_hash != contract.obligations_hash
            || disclosure.completion_contract_hash.as_deref()
                != Some(contract.fingerprint_with_policy(policy)?.as_str())
            || completion.finishing_attempts_hash
                != completed_attempts_hash_with_policy(attempts, policy)?
            || completion.assessment
                != {
                    let mut derived = super::staged_finish::derive_assessment_for_binding_with_policy_using_consistency(
                    request_hash,
                    &envelope,
                    task,
                    step,
                    ledger,
                    disclosure,
                    completion.outcome,
                    attempts,
                    policy,
                    consistency,
                )?;
                    // Only a bound capacity exclusion can re-derive a disclosure;
                    // without consistency a disclosed observation fails closed.
                    if completion
                        .assessment
                        .iter()
                        .any(|item| item.capacity_disclosed)
                    {
                        super::staged_finish::disclose_capacity(
                            &mut derived,
                            &self.limitations,
                            consistency,
                        );
                    }
                    derived
                }
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}

#[cfg(test)]
mod attempt_policy_tests {
    use super::*;

    #[test]
    fn completion_attempt_policy_preserves_ordered_hash_and_exact_capacity() {
        // AC-0266: journal identities retain their literal v1 encoding/domain.
        let policy = InvestigationPolicy::for_limits(&InvestigationLimits::default()).unwrap();
        assert_eq!(policy.max_completed_attempts(), 16);
        assert_eq!(
            completed_attempts_hash(&[]).unwrap(),
            "624d0b347af43bcc8e218043ff6e8f893be9ff0a8a8470c0cf275eb2a15d3689"
        );
        assert_eq!(
            completed_attempts_hash(&[]).unwrap(),
            completed_attempts_hash_with_policy(&[], policy).unwrap()
        );
        let values = (0..policy.max_completed_attempts())
            .map(|index| format!("{index:064x}"))
            .collect::<Vec<_>>();
        let mut ids = values.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(
            completed_attempt_ids_hash_with_policy(&ids, policy).unwrap(),
            "be42d429beea794160144a62d036c702412b96b11ff89a52f76052bac354b236"
        );
        ids.reverse();
        assert_eq!(
            completed_attempt_ids_hash_with_policy(&ids, policy).unwrap(),
            "62432f604ef3b49aa7989807c8f8293b4c44671ae9e8c39178eca3bc7d799c3b"
        );
        ids.push("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
        assert_eq!(
            completed_attempt_ids_hash_with_policy(&ids, policy),
            Err(InvestigationError::InvalidInput)
        );
        ids.pop();
        ids[1] = ids[0];
        assert_eq!(
            completed_attempt_ids_hash_with_policy(&ids, policy),
            Err(InvestigationError::InvalidInput)
        );
        ids.pop();
        ids[0] = "invalid";
        assert_eq!(
            completed_attempt_ids_hash_with_policy(&ids, policy),
            Err(InvestigationError::InvalidInput)
        );
    }
}
