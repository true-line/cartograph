//! Staged finish decisions only: no provider, public result or task transition.
use super::{obligations::*, staged_attempt::*, *};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionOutcome {
    Supported,
    InsufficientEvidence,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedFinish {
    pub schema_version: u32,
    pub outcome: CompletionOutcome,
    pub action: InvestigationAction,
}
impl StagedFinish {
    pub fn decode(raw: &str) -> Result<Self, InvestigationError> {
        strict::preflight(raw, MAX_ACTION_BYTES, 32, 4096, 32)?;
        let finish: Self =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidAction)?;
        finish.validate()?;
        Ok(finish)
    }
    fn validate(&self) -> Result<(), InvestigationError> {
        if self.schema_version != 1 {
            return Err(InvestigationError::InvalidAction);
        }
        let action = InvestigationAction::decode(
            &String::from_utf8(bounded_bytes(&self.action, MAX_ACTION_BYTES)?)
                .map_err(|_| InvestigationError::InvalidAction)?,
        )?;
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
            _ => Err(InvestigationError::InvalidAction),
        }
    }
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        self.validate()?;
        Ok(crate::source_basis::domain_hash(
            b"cartograph:obligation-finish:v1\0",
            &bounded_bytes(self, MAX_ACTION_BYTES)?,
        ))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    NotAttempted,
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
#[serde(deny_unknown_fields)]
pub struct RefusalFeedback {
    pub schema_version: u32,
    pub unmet: Vec<FeedbackEntry>,
}
impl RefusalFeedback {
    pub fn bytes(&self) -> Result<Vec<u8>, InvestigationError> {
        if self.schema_version != 1
            || self.unmet.is_empty()
            || self.unmet.len() > 12
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
        bounded_bytes(self, 2 * 1024)
    }
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        Ok(crate::source_basis::domain_hash(
            b"cartograph:obligation-feedback:v1\0",
            &self.bytes()?,
        ))
    }
}
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
    finish.validate()?;
    let request = request.normalized()?;
    let ledger = input.ledger();
    if disclosure.schema_version == 4 {
        input.validate_completion_binding(&request, disclosure, attempts)?;
    }
    if request
        .expected_graph_revision
        .as_deref()
        .is_some_and(|graph| graph != ledger.graph_snapshot_id)
        || ledger
            .coverage
            .as_ref()
            .is_none_or(|proof| proof.context_id != request.expected_context_id)
        || attempts.len() > 2 * MAX_TOOL_ACTIONS as usize
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
        evidence.record.validate(&request, task, evidence.ledger)?;
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
    let assessment = derive_assessment(
        &request,
        task,
        step,
        ledger,
        disclosure,
        finish.outcome,
        attempts,
    )?;
    let completion = if disclosure.schema_version == 4 {
        Some(InvestigationCompletionResult {
            schema_version: 1,
            outcome: finish.outcome,
            request_hash: request.intent_hash()?,
            obligations_hash: request.obligations.fingerprint()?,
            finishing_attempts_hash: completed_attempts_hash(attempts)?,
            assessment: assessment.clone(),
        })
    } else {
        None
    };
    // All security/citation/prose replay admission precedes a recoverable decision.
    let admitted = input.admit_result_bound(
        task,
        step,
        Some(disclosure),
        &finish.action,
        model,
        timestamp,
        completion,
    )?;
    let unmet = assessment
        .iter()
        .filter_map(|observation| {
            observation.unmet.map(|reason| FeedbackEntry {
                obligation_id: observation.obligation_id.clone(),
                kind: observation.kind.clone(),
                reason,
            })
        })
        .collect::<Vec<_>>();
    if unmet.is_empty() {
        Ok(FinishDecision::Accepted(AcceptedCandidate {
            outcome: finish.outcome,
            findings: admitted.findings,
            knowledge_completeness: admitted.knowledge_completeness,
            limitations: admitted.limitations,
            assessment,
        }))
    } else {
        let feedback = RefusalFeedback {
            schema_version: 1,
            unmet,
        };
        feedback.bytes()?;
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
    let envelope = envelope.normalized()?;
    disclosure.validate(ledger)?;
    if attempts.len() > 2 * MAX_TOOL_ACTIONS as usize {
        return Err(InvestigationError::LimitExceeded);
    }
    for evidence in attempts {
        evidence
            .record
            .validate_binding(request_hash, &envelope, task, evidence.ledger)?;
    }
    let sources = envelope.sources()?;
    let source_assessment = if sources.is_empty() {
        None
    } else {
        Some(InvestigationInspectionAssessment::derive(
            &sources,
            ledger,
            step,
            disclosure,
            &[],
        )?)
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
