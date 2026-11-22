//! Read-only continuation arithmetic shared with the actual metadata writer.
//! This snapshot grants no lease, retention, capacity, seal or dispatch authority.
use super::*;

pub struct MetadataContinuationRead<'a> {
    pub request: &'a StagedObligationRequest,
    pub response: &'a InvestigationResponseMeta,
    pub original_action: &'a Value,
    pub current: &'a InvestigationInput,
    pub current_prepared: &'a PreparedBoundedCompletion,
    pub candidate_ledger: &'a InvestigationInputLedger,
    pub raw_response: &'a str,
    pub kind: ContextPreviewKind,
    pub active_milliseconds: u64,
}

pub struct MetadataContinuationSnapshot {
    policy: InvestigationPolicy,
    revision: u64,
    usage: InvestigationUsage,
    admitted_usage: InvestigationUsage,
    metadata: ProtectedCompletionMetadata,
    basis: InvestigationTurnBasis,
    mode: CompletionCallMode,
    count: u32,
    next_action_id: String,
    choices: Option<std::sync::Arc<agents::investigation::acquisition_choices::AcquisitionChoices>>,
    correction: Option<agents::investigation::acquisition_choices::AuthenticatedCitationCorrection>,
}
impl MetadataContinuationSnapshot {
    pub fn attach_acquisition(
        &self,
        input: InvestigationInput,
    ) -> Result<InvestigationInput, InvestigationError> {
        match &self.choices {
            Some(choices) => {
                input.with_acquisition_correction(choices.clone(), None, self.correction.as_ref())
            }
            None => Ok(input),
        }
    }
    pub(super) fn validate_acquisition(
        &self,
        stamp: &DisclosedAdmissionState,
    ) -> Result<(), InvestigationStoreError> {
        if let Some(choices) = &self.choices {
            stamp.validate_saved_role_shape(SpecialistId::DomainAnalystV18, &self.policy)?;
            let expected = agents::investigation::AcquisitionAdmission {
                finish_patch: None,
                finish_policy_complete: self.policy.local_finish_generation()
                    && choices.attempts_complete(),
                capacity_exhausted: false,
                finish_reason: None,
                choices_hash: choices.stamp_hash()?,
                action_feedback_hash: None,
            };
            if stamp.acquisition.as_ref() != Some(&expected) {
                return Err(InvestigationStoreError::Invalid);
            }
        } else {
            stamp.validate_with_metadata(&self.metadata, &self.policy)?;
        }
        Ok(())
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn usage(&self) -> &InvestigationUsage {
        &self.usage
    }
    pub fn admitted_usage(&self) -> &InvestigationUsage {
        &self.admitted_usage
    }
    pub fn metadata(&self) -> &ProtectedCompletionMetadata {
        &self.metadata
    }
    pub fn basis(&self) -> &InvestigationTurnBasis {
        &self.basis
    }
    pub fn mode(&self) -> CompletionCallMode {
        self.mode
    }
    pub fn capacity_refusal_count(&self) -> u32 {
        self.count
    }
    pub fn next_action_id(&self) -> &str {
        &self.next_action_id
    }
}

impl JobStore {
    pub fn investigation_metadata_continuation_snapshot(
        &self,
        id: &str,
        expected_revision: u64,
        input: &MetadataContinuationRead<'_>,
    ) -> Result<MetadataContinuationSnapshot, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_next_prompt_task(&tx, id)?;
        if task.detail.summary.revision != expected_revision || task.phase != Phase::Invocation {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        let effective_policy = super::policy_for_detail(&task.detail)?;
        let policy = &effective_policy;
        let request = input.request.normalized_with_policy(policy)?;
        if request.intent_hash_with_policy(policy)? != task.intent_hash {
            return Err(InvestigationStoreError::Conflict);
        }
        request.validate_ledger_with_policy(input.candidate_ledger, policy)?;
        let authenticated = authenticate_response(
            &tx,
            &task,
            input.response,
            input.original_action,
            input.current,
            input.current_prepared,
            input.active_milliseconds,
        )?;
        let projected = derive(
            &tx,
            &task,
            input.response,
            &authenticated,
            input.candidate_ledger,
            input.raw_response,
            input.kind,
            input.active_milliseconds,
        )?;
        tx.commit()?;
        Ok(projected.snapshot)
    }
}

pub(super) struct DerivedContinuation {
    pub snapshot: MetadataContinuationSnapshot,
    pub tool: InvestigationTool,
    pub read: Option<ReadIntent>,
    pub records: Vec<AttemptRecord>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn derive(
    tx: &rusqlite::Transaction<'_>,
    task: &StoredTask,
    response: &InvestigationResponseMeta,
    authenticated: &AuthenticatedResponse,
    ledger: &InvestigationInputLedger,
    raw_response: &str,
    kind: ContextPreviewKind,
    active_milliseconds: u64,
) -> Result<DerivedContinuation, InvestigationStoreError> {
    let effective_policy = super::policy_for_detail(&task.detail)?;
    let policy = &effective_policy;
    let core = &authenticated.core;
    let call = &authenticated.call;
    let prior = &core.ledger;
    if ledger.revision
        != prior
            .revision
            .checked_add(1)
            .ok_or(InvestigationStoreError::Invalid)?
        || !ledger.queries.starts_with(&prior.queries)
        || !ledger.coverage_reads.starts_with(&prior.coverage_reads)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let context_id = &prior
        .coverage
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?
        .context_id;
    let action = ProgressiveInvestigationAction::decode_resolved_for_specialist(
        std::str::from_utf8(&authenticated.original)
            .map_err(|_| InvestigationStoreError::Invalid)?,
        context_id,
        &call.request_hash,
        prior,
        policy,
        task.detail.summary.specialist_id,
    )?;
    let (tool, read, hash, bytes, maximum) = match (kind, action) {
        (
            ContextPreviewKind::Query,
            CompletionInvestigationAction::Read(QualifiedInvestigationAction::QueryContext {
                query,
                ..
            }),
        ) if ledger.queries.len() == prior.queries.len() + 1
            && ledger.coverage_reads == prior.coverage_reads =>
        {
            let last = ledger
                .queries
                .last()
                .ok_or(InvestigationStoreError::Invalid)?;
            if last.query != query {
                return Err(InvestigationStoreError::Invalid);
            }
            (
                InvestigationTool::QueryContext,
                None,
                &last.response_hash,
                last.response_bytes,
                query.max_bytes,
            )
        }
        (
            ContextPreviewKind::Inventory,
            CompletionInvestigationAction::Read(
                QualifiedInvestigationAction::ReadCoverageInventory(request),
            ),
        ) if ledger.coverage_reads.len() == prior.coverage_reads.len() + 1
            && ledger.queries == prior.queries =>
        {
            let last = ledger
                .coverage_reads
                .last()
                .ok_or(InvestigationStoreError::Invalid)?;
            if last.request != request {
                return Err(InvestigationStoreError::Invalid);
            }
            let maximum = request.max_bytes;
            (
                InvestigationTool::ReadCoverageInventory,
                Some(ReadIntent::Inventory {
                    context_id: request.context_id,
                    repo: request.repo,
                    inventory_hash: request.inventory_hash,
                    max_bytes: request.max_bytes,
                }),
                &last.response_hash,
                last.response_bytes,
                maximum,
            )
        }
        _ => return Err(InvestigationStoreError::Invalid),
    };
    // Full typed response/protected-field validation still belongs to the real
    // candidate builder/certificate/seal. Bind these exact retained UTF-8 bytes
    // here so the preview cannot describe a different manifest or publication.
    if bytes != raw_response.len() || hash != &core_prov::content_hash(raw_response.as_bytes()) {
        return Err(InvestigationStoreError::Invalid);
    }
    if raw_response.len() > maximum || raw_response.len() > MAX_QUERY_BYTES {
        return Err(InvestigationStoreError::Capacity);
    }
    let records = match &read {
        Some(read) => completion::derive_attempt_records(
            task,
            &call.step_id,
            response,
            read,
            &AttemptOutcome::InventoryAcquired {
                read_index: prior.coverage_reads.len(),
            },
            Some(ledger),
            core.metadata.completed_attempt_ids.len(),
        )?,
        None => vec![],
    };
    let mut metadata = core.metadata.clone();
    metadata
        .completed_attempt_ids
        .extend(records.iter().map(|record| record.attempt_id.clone()));
    let mut usage = authenticated.admitted_usage.clone();
    usage.active_milliseconds = active_milliseconds;
    usage.tool_actions = usage
        .tool_actions
        .checked_add(1)
        .ok_or(InvestigationStoreError::Capacity)?;
    usage.selected_facts = ledger.selected_facts.len();
    let mut candidate = task.clone();
    candidate.detail.usage = usage.clone();
    candidate.action_tool = Some(tool);
    transitions::validate_input_candidate(tx, &candidate, ledger, &usage)?;
    let basis = InvestigationTurnBasis {
        input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
        previous_ledger: Some(prior.clone()),
        completed_tool: Some(tool),
    };
    basis.validate_with_policy(ledger, policy)?;
    let mode = metadata.next_call_mode_with_policy(&usage, policy)?;
    let choices = if task.detail.summary.specialist_id.uses_acquisition_choices() {
        completion::SavedMetadataHistory::load(tx, task)?
            .prospective_choices(ledger, &metadata, &records)?
    } else {
        None
    };
    Ok(DerivedContinuation {
        snapshot: MetadataContinuationSnapshot {
            correction: completion::next_prompt_snapshot::authenticated_citation_correction(
                tx, task, &metadata,
            )?,
            policy: effective_policy.clone(),
            revision: task.detail.summary.revision,
            usage,
            admitted_usage: authenticated.admitted_usage.clone(),
            metadata,
            basis,
            mode,
            count: call.admission.capacity_refusal_count,
            next_action_id: authenticated.next_action_id.clone(),
            choices,
        },
        tool,
        read,
        records,
    })
}

#[cfg(any(test, feature = "test-support"))]
#[path = "metadata_continuation_tests.rs"]
pub(in crate::jobs::investigations) mod tests;
