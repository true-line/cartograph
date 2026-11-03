//! Pure candidate authority, not dispatch or source access (AC-0472).
//! Callers must authenticate the contract and each attempt's durable completion.
//! No specialist uses this foundation until projection/admission wiring lands.
use super::{staged_attempt::*, staged_refusal::ProtectedCompletionMetadata, *};
use crate::{TaskFactKey, TaskRangeRole};
use serde::Serialize;
use std::collections::BTreeSet;

/// Concrete acquisition wire objects; source reads still require host guards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcquisitionAction {
    QueryContext {
        context_id: String,
        query: InvestigationQuery,
    },
    ReadEvidence {
        fact: TaskFactKey,
        role: TaskRangeRole,
        index: u32,
    },
    ReadCoverageInventory {
        context_id: String,
        repo: String,
        inventory_hash: String,
        max_bytes: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcquisitionChoice {
    pub obligation_id: String,
    pub action: AcquisitionAction,
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
        self.validate_basis(contract, metadata, ledger, policy)?;
        bounded_bytes(&invalid_ids, MAX_ACTION_BYTES)?;
        if invalid_ids.is_empty() {
            return Ok(None);
        }
        let mut targets = BTreeSet::new();
        for id in invalid_ids {
            let eligible = contract
                .obligations
                .obligations
                .iter()
                .zip(&self.pending)
                .any(|(obligation, pending)| {
                    *pending
                        && matches!(obligation, Obligation::Source { target, .. }
                            if target.target_id == *id)
                        && self
                            .entries
                            .iter()
                            .any(|entry| entry.obligation_id == obligation.key().1)
                });
            // Unknown/foreign IDs, acquired targets, and unavailable or
            // deferred targets must not become a new acquisition capability.
            if !eligible {
                return Ok(None);
            }
            targets.insert(id.clone());
        }
        Ok(Some(targets.into_iter().collect()))
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
        Self::from_pending(
            contract,
            ledger,
            metadata.fingerprint_with_policy(policy)?,
            pending,
            policy,
        )
    }

    /// Additional unencoded cache owner. Encoded choices are charged separately.
    pub fn private_workspace_bytes(policy: &InvestigationPolicy) -> Option<usize> {
        context_hub::logical::sum(&[
            std::mem::size_of::<Vec<bool>>(),
            context_hub::logical::slots::<bool>(policy.max_completion_obligations())?,
        ])
    }
    #[cfg(test)]
    pub(super) fn private_bytes(&self) -> usize {
        std::mem::size_of::<Vec<bool>>() + std::mem::size_of_val(self.pending.as_slice())
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
        let schema =
            response_schema::select_acquisition(mode, &projected, ledger, &contract.request_hash)?;
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
