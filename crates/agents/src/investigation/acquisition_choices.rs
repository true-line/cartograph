//! Pure candidate authority, not dispatch or source access (AC-0472).
//! Callers must authenticate the contract and each attempt's durable completion.
//! No specialist uses this foundation until projection/admission wiring lands.
use super::{staged_attempt::*, staged_refusal::ProtectedCompletionMetadata, *};
use crate::{TaskFactKey, TaskRangeRole};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Concrete acquisition wire objects; source reads still require host guards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AcquisitionAction {
    QueryContext {
        context_id: String,
        query: InvestigationQuery,
    },
    ReadEvidence {
        fact: TaskFactKey,
        role: TaskRangeRole,
        /// Stable source-range index for this fact and role, never menu position.
        index: u32,
    },
    ReadCoverageInventory {
        context_id: String,
        repo: String,
        inventory_hash: String,
        max_bytes: usize,
    },
}

impl AcquisitionAction {
    /// Same canonical JSON Value framing as saved model-response action hashes.
    pub fn capacity_refusal_hash(&self) -> Result<String, InvestigationError> {
        let value = serde_json::to_value(self).map_err(|_| InvestigationError::InvalidInput)?;
        Ok(core_prov::content_hash(&bounded_bytes(
            &value,
            MAX_ACTION_BYTES,
        )?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcquisitionChoice {
    pub obligation_id: String,
    pub action: AcquisitionAction,
}

/// Exact correction mappings; construction requires authenticated choices.
/// Decoding is shape-only; authenticate against the saved invocation before use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CitationCorrection {
    schema_version: u32,
    choices_hash: String,
    entries: Vec<CitationCorrectionEntry>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CitationCorrectionEntry {
    target_id: String,
    obligation_id: String,
    action: AcquisitionAction,
}

/// Invocation-bound witness. It cannot be decoded from stored JSON; reopen
/// must authenticate the saved correction before deriving fresh guidance.
#[derive(Clone)]
pub struct AuthenticatedCitationCorrection {
    correction: CitationCorrection,
    contract_hash: String,
}
impl AuthenticatedCitationCorrection {
    /// Owned validated mapping and contract fingerprint, excluding projection
    /// scratch and caller-owned history. Uses the existing feedback wire cap.
    pub fn retained_workspace_bytes(policy: &InvestigationPolicy) -> Option<usize> {
        let bytes = staged_finish::RefusalFeedback::max_bytes_with_policy(policy).ok()?;
        std::mem::size_of::<Self>()
            .checked_add(context_hub::logical::json_bytes(bytes)?)?
            .checked_add(64)
    }

    /// Reprojection/authentication scratch, including rejected candidate growth.
    /// Borrowed choices, contract, metadata and ledger are caller-owned.
    pub fn projection_workspace_bytes(policy: &InvestigationPolicy) -> Option<usize> {
        let n = policy.max_completion_obligations();
        context_hub::logical::sum(&[
            // Original IDs, deduplication tree, returned target vector and
            // projected entries coexist. Each offered action is bounded by
            // MAX_ACTION_BYTES, even before the smaller feedback check.
            context_hub::logical::slots::<String>(n)?.checked_mul(2)?,
            context_hub::logical::set::<String>(n)?,
            context_hub::logical::slots::<CitationCorrectionEntry>(n)?,
            n.checked_mul(128)?,
            context_hub::logical::json_bytes(MAX_ACTION_BYTES)?.checked_mul(3)?,
            super::task_validation_workspace::envelope_workspace(policy)?,
            ledger_validation_workspace(policy, MAX_RECORD_BYTES)?,
            context_hub::logical::json_bytes(MAX_MANIFEST_BYTES)?,
        ])
    }

    pub(super) fn original(&self) -> &CitationCorrection {
        &self.correction
    }

    /// Replace historical actions with the exact currently offered actions.
    /// Completed, blocked and deferred targets disappear from guidance; none
    /// become a capability merely because they occurred in a refused finish.
    pub fn project(
        &self,
        choices: &AcquisitionChoices,
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<Option<CitationCorrection>, InvestigationError> {
        choices.validate_basis(contract, metadata, ledger, policy)?;
        if self.contract_hash != contract.fingerprint_with_policy(policy)? {
            return Err(InvestigationError::InvalidInput);
        }
        let mut targets = Vec::new();
        for entry in &self.correction.entries {
            if choices
                .unacquired_citation_targets(
                    std::slice::from_ref(&entry.target_id),
                    contract,
                    metadata,
                    ledger,
                    policy,
                )?
                .is_some()
            {
                targets.push(entry.target_id.clone());
            }
        }
        choices.citation_correction(&targets, contract, metadata, ledger, policy)
    }
}

impl CitationCorrection {
    /// Only for an already authenticated refusal journal. This verifies the
    /// saved mapping at its invocation cutoff; the host's immutable response
    /// binding proves that this is the complete originally published ID set.
    pub(super) fn validate_stored_mapping(
        &self,
        choices: &AcquisitionChoices,
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        // Reject oversized/malformed saved mappings before cloning their IDs.
        self.bytes_with_policy(policy)?;
        let ids = self
            .entries
            .iter()
            .map(|entry| entry.target_id.clone())
            .collect::<Vec<_>>();
        self.validate(&ids, choices, contract, metadata, ledger, policy)
    }

    pub(super) fn authenticate_stored(
        &self,
        choices: &AcquisitionChoices,
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<AuthenticatedCitationCorrection, InvestigationError> {
        self.validate_stored_mapping(choices, contract, metadata, ledger, policy)?;
        Ok(AuthenticatedCitationCorrection {
            correction: self.clone(),
            contract_hash: contract.fingerprint_with_policy(policy)?,
        })
    }

    pub(super) fn validate_feedback(
        &self,
        unmet: &[staged_finish::FeedbackEntry],
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        self.bytes_with_policy(policy)?;
        if self.entries.iter().any(|entry| {
            !unmet.iter().any(|feedback| {
                feedback.obligation_id == entry.obligation_id
                    && feedback.kind == "source"
                    && feedback.reason == staged_finish::RefusalReason::NotAttempted
            })
        }) {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }

    /// Authenticate the complete mapping against a trusted refused response's
    /// invalid IDs and the choices at its saved invocation cutoff. Neither this
    /// decoded object nor its hash supplies those authorities.
    pub fn validate(
        &self,
        invalid_ids: &[String],
        choices: &AcquisitionChoices,
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        self.bytes_with_policy(policy)?;
        let expected = choices
            .citation_correction(invalid_ids, contract, metadata, ledger, policy)?
            .ok_or(InvestigationError::InvalidInput)?;
        if self != &expected {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }

    pub fn authenticate(
        &self,
        invalid_ids: &[String],
        choices: &AcquisitionChoices,
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<AuthenticatedCitationCorrection, InvestigationError> {
        self.validate(invalid_ids, choices, contract, metadata, ledger, policy)?;
        Ok(AuthenticatedCitationCorrection {
            correction: self.clone(),
            contract_hash: contract.fingerprint_with_policy(policy)?,
        })
    }

    /// Reuse the existing feedback ceiling, not a new numeric policy limit.
    /// This caps this component; the enclosing feedback and final escaped
    /// provider payload still require their own complete admission checks.
    pub fn bytes_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<Vec<u8>, InvestigationError> {
        if self.schema_version != 1
            || self.entries.is_empty()
            || self.entries.len() > policy.max_completion_obligations()
            || !crate::source_basis::hash(&self.choices_hash)
            || self.entries.iter().any(|entry| {
                !crate::source_basis::hash(&entry.target_id)
                    || !crate::source_basis::hash(&entry.obligation_id)
            })
            || self
                .entries
                .windows(2)
                .any(|pair| pair[0].target_id >= pair[1].target_id)
        {
            return Err(InvestigationError::InvalidInput);
        }
        bounded_bytes(
            self,
            staged_finish::RefusalFeedback::max_bytes_with_policy(policy)?,
        )
    }
}

/// Immutable derivation with stable envelope order. Deferred entries are a
/// byte-bound suffix, never completed/satisfied obligations. Finish is separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcquisitionChoices {
    contract_hash: String,
    input_ledger_hash: String,
    completion_metadata_hash: String,
    entries: Vec<AcquisitionChoice>,
    blocked: Vec<String>,
    deferred: usize,
    // Private derivation state, never part of the canonical choices wire/hash.
    #[serde(skip)]
    pending: Vec<bool>,
    #[serde(skip)]
    all_acquired: bool,
    #[serde(skip)]
    capacity_exhausted: bool,
    #[serde(skip)]
    capacity_excluded_obligations: Vec<String>,
}
impl AcquisitionChoices {
    /// Canonical typed JSON under the existing action cap; not a publication
    /// proof. Producers and saved readers must use these identical hash bytes.
    pub fn stamp_hash(&self) -> Result<String, InvestigationError> {
        Ok(core_prov::content_hash(&bounded_bytes(
            self,
            MAX_ACTION_BYTES,
        )?))
    }
    /// Project authenticated, turn-local capacity refusals before truncating
    /// the offered prefix. This is not journal authentication: the saved host
    /// must supply the complete refusal set at the invocation's exact cutoff.
    /// Refused obligations stay blocked/unattempted; they never become acquired.
    pub fn excluding_capacity_refusals(
        &self,
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        refused: &BTreeSet<String>,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        self.project_capacity_refusals(contract, metadata, ledger, refused, policy, None)
    }

    /// Separate unresolved-obligation projection for new feedback. The caller
    /// authenticates refusal hashes at the exact invocation cutoff, just as for
    /// the legacy menu. Generic blocked targets are deliberately not included.
    /// This does not mark an obligation attempted, acquired, or satisfied.
    pub fn excluding_capacity_refusals_with_obligations(
        &self,
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        refused: &BTreeSet<String>,
        policy: &InvestigationPolicy,
    ) -> Result<(Self, Vec<String>), InvestigationError> {
        let mut excluded = Vec::new();
        let choices = self.project_capacity_refusals(
            contract,
            metadata,
            ledger,
            refused,
            policy,
            Some(&mut excluded),
        )?;
        Ok((choices, excluded))
    }

    /// Additional owned ID projection, alongside the existing candidate bound.
    pub fn capacity_obligations_workspace_bytes(policy: &InvestigationPolicy) -> Option<usize> {
        context_hub::logical::sum(&[
            std::mem::size_of::<Vec<String>>(),
            context_hub::logical::slots::<String>(policy.max_completion_obligations())?,
            policy.max_completion_obligations().checked_mul(64)?,
        ])
    }

    #[allow(clippy::too_many_arguments)]
    fn project_capacity_refusals(
        &self,
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        refused: &BTreeSet<String>,
        policy: &InvestigationPolicy,
        excluded: Option<&mut Vec<String>>,
    ) -> Result<Self, InvestigationError> {
        self.validate_basis(contract, metadata, ledger, policy)?;
        if refused.len() > policy.limits().model_invocations as usize
            || refused.iter().any(|hash| !crate::source_basis::hash(hash))
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut projected = Self::from_pending_excluding(
            contract,
            ledger,
            self.completion_metadata_hash.clone(),
            self.pending.clone(),
            policy,
            refused,
            excluded,
        )?;
        projected.all_acquired = self.all_acquired;
        projected.capacity_exhausted =
            !refused.is_empty() && projected.entries.is_empty() && !projected.all_acquired;
        Ok(projected)
    }

    /// @22-only retained projection; the host must charge its additional owners.
    pub fn with_capacity_obligations(
        &self,
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        refused: &BTreeSet<String>,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        let (mut projected, ids) = self.excluding_capacity_refusals_with_obligations(
            contract, metadata, ledger, refused, policy,
        )?;
        projected.capacity_excluded_obligations = ids;
        Ok(projected)
    }

    pub fn capacity_excluded_obligations(&self) -> &[String] {
        &self.capacity_excluded_obligations
    }

    pub fn capacity_exhausted(&self) -> bool {
        self.capacity_exhausted
    }

    pub fn entries(&self) -> &[AcquisitionChoice] {
        &self.entries
    }
    pub fn deferred(&self) -> usize {
        self.deferred
    }
    /// Unattempted without a supported next action; never satisfied.
    pub fn blocked(&self) -> &[String] {
        &self.blocked
    }

    pub fn all_acquired(&self) -> bool {
        self.all_acquired
    }

    pub fn attempts_complete(&self) -> bool {
        self.entries.is_empty() && self.blocked.is_empty() && self.deferred == 0
    }
    pub fn validate_basis(
        &self,
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        if self.contract_hash != contract.fingerprint_with_policy(policy)?
            || self.input_ledger_hash != ledger.fingerprint_with_policy(policy)?
            || self.completion_metadata_hash != metadata.fingerprint_with_policy(policy)?
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
    /// Classify the invalid citation IDs from an authenticated finish. This is
    /// feedback eligibility only, never citation admission or source authority.
    /// The host must derive the complete invalid-ID set from the saved finish
    /// and its exact admitted citations, and enforce the once-only transition.
    pub fn unacquired_citation_targets(
        &self,
        invalid_ids: &[String],
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<Option<Vec<String>>, InvestigationError> {
        self.classify_unacquired_citations::<false>(invalid_ids, contract, metadata, ledger, policy)
    }

    pub fn unacquired_citation_handles(
        &self,
        invalid_ids: &[String],
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<Option<Vec<String>>, InvestigationError> {
        self.classify_unacquired_citations::<true>(invalid_ids, contract, metadata, ledger, policy)
    }

    fn classify_unacquired_citations<const RECEIPTS: bool>(
        &self,
        invalid_ids: &[String],
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<Option<Vec<String>>, InvestigationError> {
        self.validate_basis(contract, metadata, ledger, policy)?;
        bounded_bytes(&invalid_ids, MAX_ACTION_BYTES)?;
        if invalid_ids.is_empty() {
            return Ok(None);
        }
        let mut targets = BTreeSet::new();
        for id in invalid_ids {
            let mut matched = false;
            for (obligation, pending) in contract.obligations.obligations.iter().zip(&self.pending)
            {
                let Obligation::Source { target, .. } = obligation else {
                    continue;
                };
                if target.target_id != *id && !(RECEIPTS && target.receipt_id == *id) {
                    continue;
                }
                matched = true;
                // Every matching target must be pending and currently offered.
                // A shared receipt cannot revive an acquired or blocked target.
                if !*pending
                    || !self
                        .entries
                        .iter()
                        .any(|entry| entry.obligation_id == obligation.key().1)
                {
                    return Ok(None);
                }
                targets.insert(target.target_id.clone());
            }
            if !matched {
                return Ok(None);
            }
        }
        Ok(Some(targets.into_iter().collect()))
    }

    /// Derive model-visible correction from authenticated eligible target IDs.
    /// This is guidance only: neither a read nor new acquisition authority.
    pub fn citation_correction(
        &self,
        invalid_ids: &[String],
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<Option<CitationCorrection>, InvestigationError> {
        let Some(targets) =
            self.unacquired_citation_targets(invalid_ids, contract, metadata, ledger, policy)?
        else {
            return Ok(None);
        };
        let mut entries = Vec::with_capacity(targets.len());
        for target_id in targets {
            let obligation = contract
                .obligations
                .obligations
                .iter()
                .find(|obligation| {
                    matches!(obligation, Obligation::Source { target, .. }
                    if target.target_id == target_id)
                })
                .ok_or(InvestigationError::InvalidInput)?;
            let choice = self
                .entries
                .iter()
                .find(|entry| entry.obligation_id == obligation.key().1)
                .ok_or(InvestigationError::InvalidInput)?;
            entries.push(CitationCorrectionEntry {
                target_id,
                obligation_id: choice.obligation_id.clone(),
                action: choice.action.clone(),
            });
        }
        let correction = CitationCorrection {
            schema_version: 1,
            choices_hash: self.stamp_hash()?,
            entries,
        };
        correction.bytes_with_policy(policy)?;
        Ok(Some(correction))
    }

    pub fn permits_acquisition(&self, action: &QualifiedInvestigationAction) -> bool {
        self.attempts_complete()
            || self
                .entries
                .iter()
                .any(|entry| match (&entry.action, action) {
                    (
                        AcquisitionAction::QueryContext {
                            context_id: expected,
                            query: expected_query,
                        },
                        QualifiedInvestigationAction::QueryContext { context_id, query },
                    ) => expected == context_id && expected_query == query,
                    (
                        AcquisitionAction::ReadEvidence {
                            fact: expected,
                            role: expected_role,
                            index: expected_index,
                        },
                        QualifiedInvestigationAction::SourceOrFinish(
                            InvestigationAction::ReadEvidence { fact, role, index },
                        ),
                    ) => expected == fact && expected_role == role && expected_index == index,
                    (
                        AcquisitionAction::ReadCoverageInventory {
                            context_id,
                            repo,
                            inventory_hash,
                            max_bytes,
                        },
                        QualifiedInvestigationAction::ReadCoverageInventory(read),
                    ) => {
                        context_id == &read.context_id
                            && repo == &read.repo
                            && inventory_hash == &read.inventory_hash
                            && max_bytes == &read.max_bytes
                    }
                    _ => false,
                })
    }

    /// Exact binding is consistency checking, not authentication of local JSON
    /// or a claim that a journal transaction occurred. Attempt ledgers are the
    /// retained revisions named by their records, never reconstructed graphs.
    pub fn derive(
        contract: &InvestigationCompletionContract,
        metadata: &ProtectedCompletionMetadata,
        task: &str,
        ledger: &InvestigationInputLedger,
        attempts: &[AttemptEvidence<'_>],
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        contract.validate_with_policy(policy)?;
        metadata.validate_with_policy(policy)?;
        ledger.validate_with_policy(policy)?;
        if !super::records::opaque_id(task)
            || metadata.request_hash != contract.request_hash
            || metadata.obligations_hash != contract.obligations_hash
            || attempts.len() != metadata.completed_attempt_ids.len()
        {
            return Err(InvestigationError::InvalidInput);
        }
        ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        let mut attempted = BTreeSet::new();
        for (position, (evidence, id)) in attempts
            .iter()
            .zip(&metadata.completed_attempt_ids)
            .enumerate()
        {
            let record = evidence.record;
            if &record.attempt_id != id
                || evidence.ledger.revision > ledger.revision
                || evidence.ledger.graph_snapshot_id != ledger.graph_snapshot_id
                || evidence.ledger.scope_snapshot_id != ledger.scope_snapshot_id
            {
                return Err(InvestigationError::InvalidInput);
            }
            record.validate_binding_with_policy(
                &contract.request_hash,
                &contract.obligations,
                task,
                evidence.ledger,
                policy,
            )?;
            if let AttemptOutcome::FirewallRefused {
                acquired_attempt_id,
                ..
            } = &record.outcome
                && !attempts[..position].iter().any(|prior| {
                    prior.record.attempt_id == *acquired_attempt_id
                        && prior.record.obligation_id == record.obligation_id
                        && matches!(
                            prior.record.outcome,
                            AttemptOutcome::SourceAcquired { .. }
                                | AttemptOutcome::InventoryAcquired { .. }
                        )
                })
            {
                return Err(InvestigationError::InvalidInput);
            }
            attempted.insert(record.obligation_id.as_str());
        }
        let pending = contract
            .obligations
            .obligations
            .iter()
            .map(|item| !attempted.contains(item.key().1))
            .collect();
        let mut choices = Self::from_pending(
            contract,
            ledger,
            metadata.fingerprint_with_policy(policy)?,
            pending,
            policy,
        )?;
        // Completion records are authenticated above; require their exact
        // acquisition to remain in the current ledger as well.
        choices.all_acquired = contract.obligations.obligations.iter().all(|obligation| {
            attempts.iter().any(|evidence| {
                evidence.record.obligation_id == obligation.key().1
                    && match (&evidence.record.outcome, obligation) {
                        (
                            AttemptOutcome::SourceAcquired { citation_id },
                            Obligation::Source { target, .. },
                        ) => ledger.citations.iter().any(|citation| {
                            citation.citation_id == *citation_id && target.matches(citation)
                        }),
                        (
                            AttemptOutcome::InventoryAcquired { read_index },
                            Obligation::Inventory { target, .. },
                        ) => ledger.coverage.as_ref().is_some_and(|proof| {
                            ledger.coverage_reads.get(*read_index).is_some_and(|read| {
                                read.validate(proof).is_ok()
                                    && read.request.context_id == target.context_id
                                    && read.request.repo == target.repo
                                    && read.request.inventory_hash == target.inventory_hash
                            })
                        }),
                        _ => false,
                    }
            })
        });
        Ok(choices)
    }

    /// Additional unencoded cache owner. Encoded choices are charged separately.
    pub fn private_workspace_bytes(policy: &InvestigationPolicy) -> Option<usize> {
        context_hub::logical::sum(&[
            std::mem::size_of::<Vec<bool>>(),
            Self::capacity_obligations_workspace_bytes(policy)?,
            std::mem::size_of::<bool>().checked_mul(2)?,
            context_hub::logical::slots::<bool>(policy.max_completion_obligations())?,
        ])
    }
    #[cfg(test)]
    pub(super) fn private_bytes(&self) -> usize {
        std::mem::size_of::<Vec<bool>>()
            + 2 * std::mem::size_of::<bool>()
            + std::mem::size_of_val(self.pending.as_slice())
            + std::mem::size_of_val(&self.capacity_excluded_obligations)
            + std::mem::size_of_val(self.capacity_excluded_obligations.as_slice())
            + self
                .capacity_excluded_obligations
                .iter()
                .map(String::len)
                .sum::<usize>()
    }
    /// Candidate construction before prefix truncation. Borrowed contract,
    /// ledgers, attempt witnesses and their validation remain caller-owned.
    pub fn candidate_workspace_bytes(policy: &InvestigationPolicy) -> Option<usize> {
        let bytes = policy
            .envelope_bytes()
            .checked_add(
                policy.max_completion_obligations().checked_mul(
                    context_hub::coverage::MAX_COVERAGE_IDENTITY_BYTES
                        .checked_mul(6)?
                        .checked_add(1024)?,
                )?,
            )?
            .checked_add(1024)?;
        context_hub::logical::json_bytes(bytes)?.checked_add(Self::private_workspace_bytes(policy)?)
    }
    /// Serializer-only conditional source completion. No attempt, ledger, Input
    /// or AcquisitionChoices leaves this projection; unknown hashes stay sizing
    /// values and the real publication must independently derive its authority.
    pub(super) fn source_projection(
        &self,
        contract: &InvestigationCompletionContract,
        ledger: &InvestigationInputLedger,
        target: &InvestigationInspectionTarget,
        mode: staged_refusal::CompletionCallMode,
        policy: &InvestigationPolicy,
    ) -> Result<(serde_json::Value, Option<String>), InvestigationError> {
        if self.contract_hash != contract.fingerprint_with_policy(policy)?
            || self.input_ledger_hash != ledger.fingerprint_with_policy(policy)?
        {
            return Err(InvestigationError::InvalidInput);
        }
        let read = ReadIntent::Source {
            fact: target.fact.clone(),
            role: target.role,
            index: target.index,
        };
        let mut pending = self.pending.clone();
        for (slot, obligation) in pending.iter_mut().zip(&contract.obligations.obligations) {
            if read.matches(obligation) {
                *slot = false;
            }
        }
        let mut projected = Self::from_pending(
            contract,
            ledger,
            self.completion_metadata_hash.clone(),
            pending,
            policy,
        )?;
        projected.input_ledger_hash = "f".repeat(64);
        projected.completion_metadata_hash = "f".repeat(64);
        let schema = response_schema::select_acquisition_with_policy(
            mode,
            &projected,
            ledger,
            &contract.request_hash,
            policy,
        )?;
        let value =
            serde_json::to_value(projected).map_err(|_| InvestigationError::InvalidInput)?;
        Ok((value, schema))
    }

    fn from_pending(
        contract: &InvestigationCompletionContract,
        ledger: &InvestigationInputLedger,
        completion_metadata_hash: String,
        pending: Vec<bool>,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        Self::from_pending_excluding(
            contract,
            ledger,
            completion_metadata_hash,
            pending,
            policy,
            &BTreeSet::new(),
            None,
        )
    }

    fn from_pending_excluding(
        contract: &InvestigationCompletionContract,
        ledger: &InvestigationInputLedger,
        completion_metadata_hash: String,
        pending: Vec<bool>,
        policy: &InvestigationPolicy,
        refused: &BTreeSet<String>,
        mut excluded: Option<&mut Vec<String>>,
    ) -> Result<Self, InvestigationError> {
        if pending.len() != contract.obligations.obligations.len() {
            return Err(InvestigationError::InvalidInput);
        }
        let proof = ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        let mut output = Self {
            contract_hash: contract.fingerprint_with_policy(policy)?,
            input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
            completion_metadata_hash,
            entries: Vec::new(),
            blocked: Vec::new(),
            deferred: 0,
            pending,
            all_acquired: false,
            capacity_exhausted: false,
            capacity_excluded_obligations: Vec::new(),
        };
        // Validate every obligation, including the eventual deferred suffix.
        let byte_limit = MAX_ACTION_BYTES.min(policy.envelope_bytes());
        for (position, obligation) in contract.obligations.obligations.iter().enumerate() {
            let (_, id) = obligation.key();
            let action = match obligation {
                Obligation::Source { target, .. } => {
                    if target.graph_snapshot_id != ledger.graph_snapshot_id {
                        return Err(InvestigationError::InvalidInput);
                    }
                    let selected = ledger
                        .selected_facts
                        .iter()
                        .find(|item| item.fact == target.fact);
                    if let Some(selected) = selected
                        && (selected.fact_digest != target.fact_digest
                            || selected.binding.as_ref().is_some_and(|binding| {
                                binding.receipt_id != target.receipt_id
                                    || binding.repo_key != target.source.repo
                                    || binding.emitted_fact_digest != target.fact_digest
                            }))
                    {
                        return Err(InvestigationError::InvalidInput);
                    }
                    if selected.is_some_and(|item| item.binding.is_some()) {
                        if !ledger.receipt_references.iter().any(|reference| {
                            reference.source_id == target.registered_source_id
                                && reference.repo_key == target.source.repo
                                && reference.receipt_id == target.receipt_id
                        }) {
                            return Err(InvestigationError::InvalidInput);
                        }
                        Some(AcquisitionAction::ReadEvidence {
                            fact: target.fact.clone(),
                            role: target.role,
                            index: target.index,
                        })
                    } else if let TaskFactKey::Node { id } = &target.fact {
                        let mut query = InvestigationQuery {
                            scope: InvestigationScope::All,
                            kind: Some(InvestigationFactKind::Node),
                            labels: vec![],
                            max_facts: 1,
                            max_bytes: MAX_QUERY_BYTES,
                            cursor: None,
                        };
                        // Preserve all fixed query invariants. Only replacing
                        // the scope below may make this valid source ID
                        // ineligible for exact discovery under protocol limits.
                        query.validate_retained()?;
                        query.scope = InvestigationScope::Node { id: id.clone() };
                        if query.validate_retained().is_err()
                            || ledger.queries.iter().any(|page| page.query == query)
                        {
                            None
                        } else {
                            Some(AcquisitionAction::QueryContext {
                                context_id: proof.context_id.clone(),
                                query,
                            })
                        }
                    } else {
                        None
                    }
                }
                Obligation::Inventory { target, .. } => {
                    if target.context_id != proof.context_id
                        || !proof.qualification.repos.iter().any(|repo| {
                            repo.repo == target.repo
                                && repo.status == context_hub::coverage::CoverageStatus::Recorded
                                && repo.inventory_hash.as_deref()
                                    == Some(target.inventory_hash.as_str())
                        })
                    {
                        return Err(InvestigationError::InvalidInput);
                    }
                    Some(AcquisitionAction::ReadCoverageInventory {
                        context_id: target.context_id.clone(),
                        repo: target.repo.clone(),
                        inventory_hash: target.inventory_hash.clone(),
                        max_bytes: MAX_QUERY_BYTES,
                    })
                }
            };
            // Saved response action hashes use canonical JSON Value ordering,
            // not the enum's field order. Match that exact framing.
            let action = match action {
                Some(action) if !refused.is_empty() => {
                    let hash = action.capacity_refusal_hash()?;
                    if refused.contains(&hash) {
                        if output.pending[position]
                            && let Some(excluded) = excluded.as_deref_mut()
                        {
                            excluded.push(id.into());
                        }
                        None
                    } else {
                        Some(action)
                    }
                }
                other => other,
            };
            if output.pending[position] {
                if let Some(action) = action {
                    output.entries.push(AcquisitionChoice {
                        obligation_id: id.into(),
                        action,
                    });
                } else {
                    output.blocked.push(id.into());
                }
            }
        }
        // Existing serialized-action ceiling, further capped by the policy's
        // envelope ceiling. Blocked IDs cannot occupy actionable prefix slots,
        // but their encoded bytes still count toward this same ceiling.
        // Allocation before truncation is bounded by the contract and slot cap.
        while bounded_bytes(&output, byte_limit).is_err() {
            output
                .entries
                .pop()
                .ok_or(InvestigationError::LimitExceeded)?;
            output.deferred += 1;
        }
        Ok(output)
    }
}

#[cfg(test)]
#[path = "acquisition_choices_tests.rs"]
mod tests;

#[path = "acquisition_feedback_bound.rs"]
mod feedback_bound;
pub(super) use feedback_bound::{
    citation_projection_upper, embedded_lengths, feedback_encoded_bound,
};
