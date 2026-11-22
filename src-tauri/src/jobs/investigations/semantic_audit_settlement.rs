//! Authenticate a completed audit against the original input and durable call.
//! The receipt is transient until the owning settlement transaction saves it.
use super::*;
use agents::investigation::semantic_audit::{AuditFailure, SemanticAuditReply};
use llm::bounded::{BoundedCompletion, PreparedBoundedCompletion};

pub struct SemanticAuditResponse<'a> {
    pub input: &'a InvestigationInput,
    pub original_prepared: &'a PreparedBoundedCompletion,
    pub audit_prepared: &'a PreparedBoundedCompletion,
    pub completion: Result<&'a BoundedCompletion, AuditFailure>,
    pub active_milliseconds: u64,
}

pub(super) fn authenticate(
    tx: &rusqlite::Transaction<'_>,
    task: &StoredTask,
    response: &SemanticAuditResponse<'_>,
) -> Result<super::semantic_audit_receipt::AuditReceipt, InvestigationStoreError> {
    if task.phase != Phase::AuditInvocation {
        return Err(InvestigationStoreError::Stale);
    }
    let expected = super::semantic_audit_state::prepare_payload_for_task(
        tx,
        task,
        response.input,
        response.original_prepared,
    )?;
    let id = &task.detail.summary.investigation_id;
    let invocation =
        super::semantic_audit_journal::load(tx, id)?.ok_or(InvestigationStoreError::Invalid)?;
    let prepared = response.audit_prepared;
    let limits = prepared.limits().values();
    if !prepared.matches_action(&expected.action(invocation.action_id.clone())?)
        || prepared.preview().egress.payload_hash != invocation.payload_hash
        || prepared.profile() != &invocation.provider
        || limits != invocation.limits
        || response.active_milliseconds < task.detail.usage.active_milliseconds
    {
        return Err(InvestigationStoreError::Invalid);
    }
    // Privacy and provider identity remain hard failures. Only model output or
    // bounded transport failure may become a typed advisory not-completed state.
    let (reply, not_completed) = match response.completion {
        Ok(completion) => {
            response
                .input
                .validate_response_metadata(completion.response_model.as_deref())?;
            if completion.requested_model != invocation.provider.requested_model
                || completion.request_bytes > limits.request_bytes
                || completion.response_bytes > limits.response_bytes
            {
                return Err(InvestigationStoreError::Invalid);
            }
            if completion.text.len() as u64 > limits.output_text_bytes {
                (None, Some(AuditFailure::Length))
            } else {
                match SemanticAuditReply::decode(&completion.text) {
                    Ok(reply) => (Some(reply), None),
                    Err(_) => (None, Some(AuditFailure::InvalidOutput)),
                }
            }
        }
        Err(reason) => (None, Some(reason)),
    };
    let mut usage = InvestigationUsage {
        model_invocations: 1,
        generated_token_reservations: limits.max_output_tokens,
        active_milliseconds: response
            .active_milliseconds
            .checked_sub(invocation.pre_usage.active_milliseconds)
            .ok_or(InvestigationStoreError::Invalid)?,
        ..Default::default()
    };
    if let Some(reported) = response
        .completion
        .ok()
        .and_then(|completion| completion.usage.as_ref())
    {
        usage.reported_input_tokens = reported.input_tokens;
        usage.reported_output_tokens = reported.output_tokens;
        usage.reported_cache_read_tokens = reported.cache_read_input_tokens;
    }
    let mut receipt = super::semantic_audit_receipt::AuditReceipt {
        schema_version: invocation.schema_version,
        task_id: id.clone(),
        invocation_hash: core_prov::content_hash(&encode(&invocation, MAX_MANIFEST_BYTES)?),
        reply,
        not_completed,
        auditor_usage: usage,
    };
    let audit = task
        .semantic_audit
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let candidate = audit
        .candidate
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let ids = response
        .input
        .ledger()
        .citations
        .iter()
        .filter(|citation| !matches!(citation.origin, InvestigationEvidenceOrigin::GraphMetadata))
        .map(|citation| citation.citation_id.clone())
        .collect();
    // A well-shaped but unbound model reply is invalid output, never evidence.
    if let Some(reply) = &receipt.reply
        && reply
            .validate_bound(
                candidate,
                &invocation.candidate_hash,
                &invocation.acquired_ledger_hash,
                &invocation.contract_hash,
                &ids,
            )
            .is_err()
    {
        receipt.reply = None;
        receipt.not_completed = Some(AuditFailure::InvalidOutput);
    }
    receipt.validate(&invocation, candidate, &ids)?;
    // Validate aggregate settlement now, including terminal overrun accounting.
    audit
        .account
        .stop_auditor(&receipt.auditor_usage, &task.detail.limits)?;
    Ok(receipt)
}

impl JobStore {
    /// Persist advisory outcome and the timely, uncancelled candidate. Historical
    /// contracts continue to require a pass. No retry, repair, or new provider
    /// call is authorized by this writer.
    pub fn settle_next_prompt_semantic_audit(
        &mut self,
        execution: &JobExecution,
        revision: u64,
        response: &SemanticAuditResponse<'_>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let id = transitions::task_id_for_job(&tx, execution.id())?
            .ok_or(InvestigationStoreError::Missing)?;
        let mut task = load_next_prompt_task(&tx, &id)?;
        transitions::check_identity(&task, execution)?;
        // A cancelled call still owes a durable outcome; this grants no dispatch.
        transitions::check_job(&tx, execution, true)?;
        if task.detail.summary.revision != revision {
            return Err(InvestigationStoreError::Stale);
        }
        let receipt = authenticate(&tx, &task, response)?;
        let ledger = load_ledger(&tx, &task)?.ok_or(InvestigationStoreError::Invalid)?;
        let ids = ledger
            .citations
            .iter()
            .filter(|c| !matches!(c.origin, InvestigationEvidenceOrigin::GraphMetadata))
            .map(|c| c.citation_id.clone())
            .collect();
        let audit = task
            .semantic_audit
            .as_mut()
            .ok_or(InvestigationStoreError::Invalid)?;
        let candidate = audit
            .candidate
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?
            .clone();
        let disposition = super::semantic_audit_receipt::insert(&tx, &receipt, &candidate, &ids)?;
        audit.account = audit
            .account
            .stop_auditor(&receipt.auditor_usage, &task.detail.limits)?;
        task.detail.usage = audit.account.usage()?;
        let timely = task.detail.usage.active_milliseconds
            < task
                .detail
                .limits
                .active_seconds
                .checked_mul(1000)
                .ok_or(InvestigationStoreError::Capacity)?;
        let publish = (audit.is_advisory()
            || disposition == agents::investigation::semantic_audit::AuditDisposition::Pass)
            && timely
            && !task.detail.summary.cancel_requested;
        if publish {
            let step_id = candidate
                .invocation_step_id
                .as_ref()
                .ok_or(InvestigationStoreError::Invalid)?;
            let step: PendingInvestigationStep =
                read_body(&tx, "investigation_invocations", "step_id", &id, step_id)?
                    .ok_or(InvestigationStoreError::Invalid)?;
            let basis = admission_journal::AuthenticatedCompletionBasis::load(&tx, &task, step_id)?;
            completion::accepted(&tx, &mut task, &candidate, &ledger, &step, basis.as_ref())?;
            let body = encode(&candidate, MAX_RESULT_BYTES)?;
            let hash = core_prov::content_hash(&body);
            one(tx.execute(
                "INSERT INTO investigation_results VALUES (?1,?2,?3)",
                params![id, body, hash],
            )?)?;
            chat::insert_result_reference(&tx, &id, &candidate.result_id, &hash)?;
            task.detail.summary.has_result = true;
            task.detail.summary.status = InvestigationStatus::Completed;
        } else {
            task.detail.summary.status = if task.detail.summary.cancel_requested {
                InvestigationStatus::Cancelled
            } else {
                InvestigationStatus::Failed
            };
            task.detail.error =
                Some("The candidate was not published after semantic audit.".into());
            task.phase = Phase::Terminal;
            completion::refresh_observation(&tx, &mut task)?;
        }
        task.phase = Phase::Terminal;
        task.detail.advisory_audit = receipt.advisory()?;
        bump(&tx, &mut task)?;
        let event_kind = if publish {
            InvestigationEventKind::ResultPersisted
        } else if task.detail.summary.cancel_requested {
            InvestigationEventKind::Cancelled
        } else {
            InvestigationEventKind::Failed
        };
        event(
            &tx,
            &mut task,
            event_kind,
            None,
            None,
            if publish {
                "The candidate was persisted with advisory model feedback; this is not verification."
            } else {
                "Semantic audit ended without publishing a candidate."
            },
        )?;
        transitions::sync_terminal_job(&tx, &task)?;
        save_next_prompt_task(&tx, &task, revision, true)?;
        load_next_prompt_task(&tx, &id)?;
        tx.commit()?;
        Ok(task.detail)
    }
}
