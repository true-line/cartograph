//! Candidate storage inside the owning task transaction. No accepted result or
//! chat reference exists until a separately authenticated audit permits it.
use super::*;
use agents::investigation::semantic_audit::SemanticAuditAccount;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredSemanticAudit {
    schema_version: u32,
    pub(super) account: SemanticAuditAccount,
    pub(super) candidate: Option<InvestigationResult>,
    candidate_hash: Option<String>,
}

pub struct SemanticAuditStart<'a> {
    pub separate_auditor: Option<&'a super::semantic_audit_journal::SeparateAuditor>,
    pub input: &'a InvestigationInput,
    pub original_prepared: &'a llm::bounded::PreparedBoundedCompletion,
    pub prepared: &'a llm::bounded::PreparedBoundedCompletion,
    pub authorized: &'a llm::bounded::AuthorizedBoundedCompletion,
    pub active_milliseconds: u64,
}

impl JobStore {
    /// Reconstruct the audit payload from the retained finishing input only
    /// after authenticating its durable candidate and exact sealed disclosure.
    /// Read-only: this grants no dispatch and consumes no reserved invocation.
    pub fn prepare_next_prompt_semantic_audit(
        &mut self,
        execution: &JobExecution,
        revision: u64,
        input: &InvestigationInput,
        original_prepared: &llm::bounded::PreparedBoundedCompletion,
    ) -> Result<
        (
            agents::investigation::semantic_audit::PreparedSemanticAudit,
            llm::bounded::CompletionLimits,
        ),
        InvestigationStoreError,
    > {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self.conn.transaction()?;
        validate(&tx, &self.namespace)?;
        let id = transitions::task_id_for_job(&tx, execution.id())?
            .ok_or(InvestigationStoreError::Missing)?;
        let task = load_next_prompt_task(&tx, &id)?;
        transitions::check_identity(&task, execution)?;
        transitions::check_job(&tx, execution, false)?;
        if task.detail.summary.revision != revision || task.phase != Phase::AuditPending {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        let payload = prepare_payload_for_task(&tx, &task, input, original_prepared)?;
        let account = &task
            .semantic_audit
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?
            .account;
        let mut values = original_prepared.limits().values();
        values.max_output_tokens = account.output_tokens(&task.detail.limits);
        let limits = llm::bounded::CompletionLimits::from_audit_values(values)
            .map_err(|_| InvestigationStoreError::Invalid)?;
        Ok((payload, limits))
    }

    /// Commit the one reserved audit invocation before the worker can dispatch.
    /// Revalidate the original input and the exact firewall authorization inside
    /// this transaction; a failed write leaves both journal and charge absent.
    pub fn begin_next_prompt_semantic_audit(
        &mut self,
        execution: &JobExecution,
        revision: u64,
        start: &SemanticAuditStart<'_>,
        validate_retained: impl FnOnce(
            &crate::sources::RetainedSourceView<'_>,
        ) -> Result<(), InvestigationStoreError>,
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
        transitions::check_job(&tx, execution, false)?;
        if task.detail.summary.revision != revision || task.phase != Phase::AuditPending {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        let expected = prepare_payload_for_task(&tx, &task, start.input, start.original_prepared)?;
        if let Some(selected) = start.separate_auditor {
            selected.validate_against_model(&start.original_prepared.profile().requested_model)?;
            if &selected.profile != start.prepared.profile() {
                return Err(InvestigationStoreError::Invalid);
            }
        }
        let preview = start.prepared.preview();
        let authorized = start.authorized;
        if preview.egress.payload != *expected.payload()
            || preview.egress.action_id != format!("{id}:audit-1")
            || preview.egress.tier != llm::AnalysisTier::Agentic
            || authorized.payload() != &preview.egress.payload
            || authorized.payload_hash() != preview.egress.payload_hash
            || authorized.action_id() != preview.egress.action_id
            || authorized.tier() != preview.egress.tier
            || authorized.profile() != &preview.profile
            || authorized.limits() != &preview.limits
            || start.active_milliseconds < task.detail.usage.active_milliseconds
            || start.active_milliseconds
                >= task
                    .detail
                    .limits
                    .active_seconds
                    .checked_mul(1000)
                    .ok_or(InvestigationStoreError::Capacity)?
        {
            return Err(InvestigationStoreError::Invalid);
        }
        validate_retained(&crate::sources::RetainedSourceView::from_transaction(&tx))?;
        let mut pre_usage = task.detail.usage.clone();
        pre_usage.active_milliseconds = start.active_milliseconds;
        let audit = task
            .semantic_audit
            .as_mut()
            .ok_or(InvestigationStoreError::Invalid)?;
        audit.account = audit
            .account
            .analyst_usage(&pre_usage, &task.detail.limits)?
            .begin_audit(&task.detail.limits)?;
        let candidate = audit
            .candidate
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        let record = super::semantic_audit_journal::AuditInvocation {
            schema_version: audit.schema_version,
            task_id: id.clone(),
            finishing_step_id: candidate
                .invocation_step_id
                .clone()
                .ok_or(InvestigationStoreError::Invalid)?,
            candidate_hash: expected.candidate_hash().into(),
            acquired_ledger_hash: candidate.input_ledger_hash.clone(),
            contract_hash: expected.contract_hash().into(),
            action_id: authorized.action_id().into(),
            payload_hash: authorized.payload_hash().into(),
            provider: authorized.profile().clone(),
            separate_auditor: start.separate_auditor.cloned(),
            limits: authorized.limits().values(),
            pre_usage,
        };
        super::semantic_audit_journal::insert(&tx, &record)?;
        task.detail.usage = audit.account.usage()?;
        task.phase = Phase::AuditInvocation;
        bump(&tx, &mut task)?;
        storage::save_started_audit_task(&tx, &task, revision)?;
        // Full replay must accept the aggregate debit before it becomes durable.
        load_next_prompt_task(&tx, &id)?;
        tx.commit()?;
        Ok(task.detail)
    }

    /// Internal worker boundary, not a public task mutation or execution grant.
    /// Install only on a fresh local @20 task before its first model invocation.
    /// The worker must reserve the complete audit workspace before calling this.
    pub fn enable_next_prompt_semantic_audit(
        &mut self,
        execution: &JobExecution,
        revision: u64,
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
        transitions::check_job(&tx, execution, false)?;
        if task.detail.summary.revision != revision {
            return Err(InvestigationStoreError::Stale);
        }
        if task.phase != Phase::Ready
            || task.detail.summary.cancel_requested
            || !task
                .detail
                .summary
                .specialist_id
                .uses_typed_citation_correction()
            || task.detail.provider.mode != InvestigationProviderMode::Local
            || task.detail.usage.model_invocations != 0
            || task.semantic_audit.is_some()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        task.semantic_audit = Some(StoredSemanticAudit {
            schema_version: 2,
            account: SemanticAuditAccount::new(&task.detail.limits)?
                .analyst_usage(&task.detail.usage, &task.detail.limits)?,
            candidate: None,
            candidate_hash: None,
        });
        bump(&tx, &mut task)?;
        save_next_prompt_task(&tx, &task, revision, false)?;
        tx.commit()?;
        Ok(task.detail)
    }
}

pub(super) fn prepare_payload_for_task(
    connection: &Connection,
    task: &StoredTask,
    input: &InvestigationInput,
    original_prepared: &llm::bounded::PreparedBoundedCompletion,
) -> Result<agents::investigation::semantic_audit::PreparedSemanticAudit, InvestigationStoreError> {
    let id = &task.detail.summary.investigation_id;
    let audit = task
        .semantic_audit
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    if !audit.is_advisory() {
        // Historical definitions may reopen but never acquire new contract semantics.
        return Err(InvestigationStoreError::Invalid);
    }
    let candidate = audit
        .candidate
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let step_id = candidate
        .invocation_step_id
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let step: PendingInvestigationStep = read_body(
        connection,
        "investigation_invocations",
        "step_id",
        id,
        step_id,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    let policy = super::policy_for_detail(&task.detail)?;
    let ledger = load_ledger(connection, task)?.ok_or(InvestigationStoreError::Invalid)?;
    if input.ledger() != &ledger
        || original_prepared.preview().egress.payload_hash != step.payload_hash
        || original_prepared.preview().egress.action_id != format!("{id}:{step_id}")
        || input.seal_disclosure_with_policy(original_prepared, &policy)? != step.disclosure
    {
        return Err(InvestigationStoreError::Conflict);
    }
    let prepared = input.semantic_audit_payload(candidate, &policy)?;
    if Some(prepared.candidate_hash()) != audit.candidate_hash.as_deref() {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(prepared)
}

impl StoredSemanticAudit {
    pub(super) fn is_advisory(&self) -> bool {
        self.schema_version == 2
    }

    /// Authenticate retained candidate fields against the immutable finishing
    /// response and the same saved disclosure/attempt basis as accepted results.
    pub(super) fn validate_saved(
        &self,
        connection: &Connection,
        task: &StoredTask,
    ) -> Result<(), InvestigationStoreError> {
        self.validate(task)?;
        validate_invocation_journal(connection, task)?;
        let Some(candidate) = &self.candidate else {
            return Ok(());
        };
        let id = &task.detail.summary.investigation_id;
        let step_id = candidate
            .invocation_step_id
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        let step: PendingInvestigationStep = read_body(
            connection,
            "investigation_invocations",
            "step_id",
            id,
            step_id,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        let response: InvestigationResponseMeta = read_body(
            connection,
            "investigation_responses",
            "step_id",
            id,
            step_id,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        let basis =
            admission_journal::AuthenticatedCompletionBasis::load(connection, task, step_id)?;
        if step.step_id != *step_id
            || response.step_id != *step_id
            // Ordinary candidates reproduce the original action. A projected
            // candidate cannot: validate_result below authenticates its separate
            // host witness against this independently loaded response and prior
            // correction, including the projected result hash.
            || (candidate.completion.as_ref().and_then(|c| c.withholding.as_ref()).is_none()
                && basis.as_ref().is_none_or(|b| b.patch().is_none())
                && response.action_hash != candidate_action_hash(candidate)?)
            || response.observed_model != candidate.observed_response_model
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let ledger = load_ledger(connection, task)?.ok_or(InvestigationStoreError::Invalid)?;
        validate_step_basis(connection, task, &step)?;
        validate_result_basis(
            candidate,
            &ledger,
            &step,
            &task.detail.limits,
            basis.as_ref(),
        )?;
        completion::validate_result(connection, task, candidate, &ledger, &step, basis.as_ref())?;
        validate_inspection_result(task, candidate)?;
        let ids = ledger
            .citations
            .iter()
            .filter(|c| !matches!(c.origin, InvestigationEvidenceOrigin::GraphMetadata))
            .map(|c| c.citation_id.clone())
            .collect();
        let receipt = super::semantic_audit_receipt::load(connection, id, candidate, &ids)?;
        if let Some((receipt, disposition)) = receipt {
            if task.phase != Phase::Terminal
                || !self.account.is_terminal()
                || self.account.usage()? != task.detail.usage
            {
                return Err(InvestigationStoreError::Invalid);
            }
            let invocation = super::semantic_audit_journal::load(connection, id)?
                .ok_or(InvestigationStoreError::Invalid)?;
            let expected = self
                .account
                .empty_for_replay(&task.detail.limits)?
                .analyst_usage(&invocation.pre_usage, &task.detail.limits)?
                .begin_audit(&task.detail.limits)?
                .stop_auditor(&receipt.auditor_usage, &task.detail.limits)?;
            if expected.usage()? != task.detail.usage
                || task.detail.advisory_audit != receipt.advisory()?
            {
                return Err(InvestigationStoreError::Invalid);
            }
            let publish = (self.is_advisory()
                || disposition == agents::investigation::semantic_audit::AuditDisposition::Pass)
                && !task.detail.summary.cancel_requested
                && task.detail.usage.active_milliseconds
                    < task
                        .detail
                        .limits
                        .active_seconds
                        .checked_mul(1000)
                        .ok_or(InvestigationStoreError::Capacity)?;
            let expected_status = if publish {
                InvestigationStatus::Completed
            } else if task.detail.summary.cancel_requested {
                InvestigationStatus::Cancelled
            } else {
                InvestigationStatus::Failed
            };
            if task.detail.summary.has_result != publish
                || task.detail.summary.status != expected_status
            {
                return Err(InvestigationStoreError::Invalid);
            }
            if publish && load_result(connection, task)?.as_ref() != Some(candidate) {
                return Err(InvestigationStoreError::Invalid);
            }
        } else if task.detail.advisory_audit.is_some()
            || task.detail.summary.has_result
            || task.detail.summary.status == InvestigationStatus::Completed
        {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }

    pub(super) fn validate(&self, task: &StoredTask) -> Result<(), InvestigationStoreError> {
        if !matches!(self.schema_version, 1 | 2)
            || (task.detail.advisory_audit.is_some()
                && (!self.is_advisory()
                    || self.candidate.is_none()
                    || task.phase != Phase::Terminal))
            || !task
                .detail
                .summary
                .specialist_id
                .uses_typed_citation_correction()
            || task.detail.provider.mode != InvestigationProviderMode::Local
            || self.candidate.is_some() != self.candidate_hash.is_some()
            || (self.account.audit_started() && self.candidate.is_none())
            || (task.phase == Phase::AuditInvocation && !self.account.audit_started())
            || (task.phase == Phase::AuditPending && self.account.audit_started())
            || self.account.usage()? != task.detail.usage
        {
            return Err(InvestigationStoreError::Invalid);
        }
        self.account.validate(&task.detail.limits)?;
        if let Some(candidate) = &self.candidate {
            if self.candidate_hash.as_deref()
                != Some(core_prov::content_hash(&encode(candidate, MAX_RESULT_BYTES)?).as_str())
                || candidate.investigation_id != task.detail.summary.investigation_id
                || Some(&candidate.input_ledger_hash) != task.ledger_hash.as_ref()
                || candidate
                    .completion
                    .as_ref()
                    .is_none_or(|c| match c.outcome {
                        CompletionOutcome::Supported => c.withholding.is_some(),
                        CompletionOutcome::FindingsWithheldAfterCorrection => {
                            !uses_finding_withholding(task.detail.summary.specialist_id)
                                || c.withholding.is_none()
                        }
                        _ => true,
                    })
                || self.account.usage()? != task.detail.usage
                || (task.detail.summary.has_result
                    && (!self.account.is_terminal() || !self.account.audit_started()))
                || !matches!(
                    task.phase,
                    Phase::AuditPending | Phase::AuditInvocation | Phase::Terminal
                )
            {
                return Err(InvestigationStoreError::Invalid);
            }
        } else if matches!(task.phase, Phase::AuditPending | Phase::AuditInvocation) {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
}

/// Analyst replay must use its own immutable debit, not the aggregate which
/// includes the independent auditor. Before dispatch, ordinary writers may be
/// updating task usage; the account is synchronized at their save boundary.
pub(super) fn analyst_counters(task: &StoredTask) -> &InvestigationUsage {
    match &task.semantic_audit {
        Some(audit) if audit.account.audit_started() => audit.account.analyst_counters(),
        _ => &task.detail.usage,
    }
}

/// An aggregate extra call must have a matching immutable audit witness. This
/// check does not replace candidate replay or grant permission to dispatch.
pub(super) fn validate_invocation_journal(
    connection: &Connection,
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    let record =
        super::semantic_audit_journal::load(connection, &task.detail.summary.investigation_id)?;
    let started = task
        .semantic_audit
        .as_ref()
        .is_some_and(|a| a.account.audit_started());
    if started != record.is_some() {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(record) = record {
        let audit = task
            .semantic_audit
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        audit.validate(task)?;
        let candidate = audit
            .candidate
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        let provider = &task.detail.provider;
        let mut limits = llm::bounded::CompletionLimits::default().values();
        limits.max_output_tokens = audit.account.output_tokens(&task.detail.limits);
        limits.max_spans = u32::try_from(task.detail.limits.evidence_items)
            .map_err(|_| InvestigationStoreError::Invalid)?;
        limits.request_timeout_ms = task
            .completion_intent
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?
            .effective_request_timeout_ms();
        if record.schema_version != audit.schema_version
            || audit.is_advisory()
                != (record.contract_hash
                    == agents::investigation::semantic_audit::advisory_contract_hash()?)
            || Some(record.candidate_hash.as_str()) != audit.candidate_hash.as_deref()
            || record.acquired_ledger_hash != candidate.input_ledger_hash
            || Some(&record.finishing_step_id) != candidate.invocation_step_id.as_ref()
            || &record.pre_usage != audit.account.analyst_counters()
            || match &record.separate_auditor {
                Some(selected) => {
                    selected.profile != record.provider
                        || selected.validate_against_model(&provider.model).is_err()
                }
                None => {
                    record.provider.provider_id != provider.provider_id
                        || record.provider.requested_model != provider.model
                        || record.provider.endpoint_id != provider.endpoint
                        || Some(record.provider.protocol_version.as_str())
                            != provider.protocol_version.as_deref()
                }
            }
            || record.limits != limits
        {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    Ok(())
}

/// Called only by owner-death recovery while holding the recovered OS lock.
/// Retain the dispatch debit and all recorded usage, with no invented sample.
pub(super) fn interrupt_started_audit(
    task: &mut StoredTask,
) -> Result<(), InvestigationStoreError> {
    if task.phase != Phase::AuditInvocation {
        return Err(InvestigationStoreError::Invalid);
    }
    let audit = task
        .semantic_audit
        .as_mut()
        .ok_or(InvestigationStoreError::Invalid)?;
    audit.account = audit.account.interrupt_audit(&task.detail.limits)?;
    task.detail.usage = audit.account.usage()?;
    Ok(())
}

/// Settle a started audit without an admitted response. Preserve the clock
/// sample and reserved invocation; provider-reported totals remain unknown.
pub(super) fn stop_started_audit(task: &mut StoredTask) -> Result<(), InvestigationStoreError> {
    if task.phase != Phase::AuditInvocation {
        return Err(InvestigationStoreError::Invalid);
    }
    let audit = task
        .semantic_audit
        .as_mut()
        .ok_or(InvestigationStoreError::Invalid)?;
    let usage = InvestigationUsage {
        model_invocations: 1,
        generated_token_reservations: audit.account.output_tokens(&task.detail.limits),
        active_milliseconds: task
            .detail
            .usage
            .active_milliseconds
            .checked_sub(audit.account.analyst_counters().active_milliseconds)
            .ok_or(InvestigationStoreError::Invalid)?,
        ..Default::default()
    };
    audit.account = audit.account.stop_auditor(&usage, &task.detail.limits)?;
    task.detail.usage = audit.account.usage()?;
    Ok(())
}

/// Keep the account in the same compare-and-set write as ordinary analyst
/// usage. All existing task3 writers pass through this persistence boundary.
/// The caller owns the transaction; an exceeded reservation rolls it back.
pub(super) fn account_analyst_save(task: &mut StoredTask) -> Result<(), InvestigationStoreError> {
    let Some(audit) = task.semantic_audit.as_mut() else {
        return Ok(());
    };
    if audit.account.is_terminal() {
        if task.phase != Phase::Terminal || audit.account.usage()? != task.detail.usage {
            return Err(InvestigationStoreError::Invalid);
        }
        return Ok(());
    }
    // Cancellation may change task metadata after dispatch, but cannot feed
    // aggregate counters back into the frozen analyst account.
    if audit.account.audit_started() {
        if task.phase != Phase::AuditInvocation || audit.account.usage()? != task.detail.usage {
            return Err(InvestigationStoreError::Invalid);
        }
        return Ok(());
    }
    audit.account = if task.phase == Phase::Terminal {
        audit
            .account
            .stop_analyst(&task.detail.usage, &task.detail.limits)?
    } else {
        audit
            .account
            .analyst_usage(&task.detail.usage, &task.detail.limits)?
    };
    Ok(())
}

/// Reconstruct only model-authored fields, preserving order and canonical JSON
/// value encoding used by the original finish response's action_hash.
fn candidate_action_hash(
    candidate: &InvestigationResult,
) -> Result<String, InvestigationStoreError> {
    let finish = staged_finish::StagedFinish {
        schema_version: if candidate.narrative.is_some() { 2 } else { 1 },
        outcome: candidate
            .completion
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?
            .outcome,
        narrative: candidate.narrative.clone(),
        action: InvestigationAction::Finish {
            findings: candidate
                .findings
                .iter()
                .map(|finding| ProposedInvestigationFinding {
                    claim_kind: finding.claim_kind,
                    title: finding.title.clone(),
                    statement: finding.statement.clone(),
                    citation_ids: finding.citation_ids.clone(),
                    limitations: finding.limitations.clone(),
                })
                .collect(),
            knowledge_completeness: candidate.knowledge_completeness,
            limitations: candidate.limitations.clone(),
        },
    };
    finish.fingerprint()?;
    let value = serde_json::to_value(finish).map_err(|_| InvestigationStoreError::Invalid)?;
    Ok(core_prov::content_hash(&encode(&value, MAX_ACTION_BYTES)?))
}

/// Called only after the finish writer authenticated the original response,
/// source ledger and structural result. Never accepts a caller candidate DTO.
pub(super) fn hold_candidate(
    task: &mut StoredTask,
    candidate: InvestigationResult,
    usage: &InvestigationUsage,
) -> Result<(), InvestigationStoreError> {
    let audit = task
        .semantic_audit
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    if audit.candidate.is_some() || task.detail.summary.cancel_requested {
        return Err(InvestigationStoreError::Invalid);
    }
    let account = audit.account.analyst_usage(usage, &task.detail.limits)?;
    let candidate_hash = core_prov::content_hash(&encode(&candidate, MAX_RESULT_BYTES)?);
    task.semantic_audit = Some(StoredSemanticAudit {
        schema_version: audit.schema_version,
        account,
        candidate: Some(candidate),
        candidate_hash: Some(candidate_hash),
    });
    // Admission has resolved the one-turn merge. An audit candidate is not a
    // pending patch and must never grant patch resumption on reopen.
    task.pending_correction = None;
    task.phase = Phase::AuditPending;
    task.semantic_audit.as_ref().unwrap().validate(task)
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn fixture_enabled_account(
    task: &StoredTask,
) -> Result<StoredSemanticAudit, InvestigationStoreError> {
    Ok(StoredSemanticAudit {
        schema_version: 2,
        account: SemanticAuditAccount::new(&task.detail.limits)?
            .analyst_usage(&task.detail.usage, &task.detail.limits)?,
        candidate: None,
        candidate_hash: None,
    })
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;

    #[test]
    fn audit_reserved_call_failure_rolls_back_task_writer() {
        use completion::next_prompt_snapshot::tests::fixture::Fixture as Acquisition;
        let base = Acquisition::configured_request(
            false,
            |request, _| request.specialist_id = "domain-analyst@20".into(),
            |_| {},
            None,
        );
        let mut f = completion::FinishFixture::from_base(base, false);
        let revision = f.saved().detail.summary.revision;
        let enabled = f
            .base
            .f
            .store
            .enable_next_prompt_semantic_audit(&f.base.f.execution, revision)
            .unwrap();
        assert!(
            f.base
                .f
                .store
                .enable_next_prompt_semantic_audit(&f.base.f.execution, revision)
                .is_err()
        );
        assert!(
            f.base
                .f
                .store
                .enable_next_prompt_semantic_audit(&f.base.f.execution, enabled.summary.revision)
                .is_err()
        );
        let saved = f.saved();
        let before = encode(&saved, MAX_RECORD_BYTES).unwrap();
        #[cfg(unix)]
        {
            // AC-0566: the enabled writer owns the caller, replacement and
            // encoded record simultaneously; check the genuine saved account.
            let actual_task = std::mem::size_of_val(&saved) + before.len();
            assert!(
                next_prompt_task_write_workspace().unwrap()
                    >= actual_task * 2 + before.len() + next_prompt_contract_workspace().unwrap()
            );
        }
        let mut over = saved.clone();
        over.detail.usage.model_invocations = over.detail.limits.model_invocations;
        {
            let tx = f.base.f.store.conn.transaction().unwrap();
            bump(&tx, &mut over).unwrap();
            assert!(
                save_next_prompt_task(&tx, &over, saved.detail.summary.revision, false).is_err()
            );
            tx.rollback().unwrap();
        }
        assert_eq!(encode(&f.saved(), MAX_RECORD_BYTES).unwrap(), before);
    }

    // AC-0566: initialization is not a candidate or provider grant.
    #[test]
    fn audit_preparation_requires_pending_candidate_without_writing() {
        use completion::next_prompt_snapshot::tests::fixture::Fixture as Acquisition;
        let base = Acquisition::configured_request(
            false,
            |request, _| request.specialist_id = "domain-analyst@20".into(),
            |_| {},
            None,
        );
        let mut f = completion::FinishFixture::from_base(base, false);
        let revision = f.saved().detail.summary.revision;
        let detail = f
            .base
            .f
            .store
            .enable_next_prompt_semantic_audit(&f.base.f.execution, revision)
            .unwrap();
        let before = f.rows();
        let changes = f.base.f.store.conn.total_changes();
        assert!(matches!(
            f.base.f.store.prepare_next_prompt_semantic_audit(
                &f.base.f.execution,
                detail.summary.revision,
                &f.input,
                &f.prepared,
            ),
            Err(InvestigationStoreError::Stale)
        ));
        let authorized = next_prompt_invocation::tests::firewall()
            .authorize_bounded(
                &next_prompt_invocation::tests::provider(false),
                &f.prepared,
                None,
            )
            .unwrap();
        assert!(matches!(
            f.base.f.store.begin_next_prompt_semantic_audit(
                &f.base.f.execution,
                detail.summary.revision,
                &crate::jobs::investigations::SemanticAuditStart {
                    separate_auditor: None,
                    input: &f.input,
                    original_prepared: &f.prepared,
                    prepared: &f.prepared,
                    authorized: &authorized,
                    active_milliseconds: 0,
                },
                |_| panic!("no retained-source callback before candidate admission"),
            ),
            Err(InvestigationStoreError::Stale)
        ));
        assert_eq!(f.base.f.store.conn.total_changes(), changes);
        assert_eq!(f.rows(), before);
        assert_eq!(f.saved().detail.usage.model_invocations, 0);
        assert!(
            super::super::semantic_audit_journal::load(&f.base.f.store.conn, f.base.id(),)
                .unwrap()
                .is_none()
        );
    }

    // AC-0566: neither mutable counters nor an orphan journal authorize a call.
    #[test]
    fn audit_replay_requires_matching_charge_and_immutable_invocation() {
        use completion::next_prompt_snapshot::tests::fixture::Fixture as Acquisition;
        let base = Acquisition::configured_request(
            false,
            |request, _| request.specialist_id = "domain-analyst@20".into(),
            |_| {},
            None,
        );
        let mut f = completion::FinishFixture::from_base(base, false);
        let revision = f.saved().detail.summary.revision;
        f.base
            .f
            .store
            .enable_next_prompt_semantic_audit(&f.base.f.execution, revision)
            .unwrap();
        let task = f.saved();
        assert_eq!(analyst_counters(&task), &task.detail.usage);
        validate_invocation_journal(&f.base.f.store.conn, &task).unwrap();
        let mut forged = task.clone();
        let audit = forged.semantic_audit.as_mut().unwrap();
        audit.account = audit.account.begin_audit(&forged.detail.limits).unwrap();
        forged.detail.usage = audit.account.usage().unwrap();
        assert_eq!(forged.detail.usage.model_invocations, 1);
        assert_eq!(analyst_counters(&forged).model_invocations, 0);
        assert!(validate_invocation_journal(&f.base.f.store.conn, &forged).is_err());
        let record = super::super::semantic_audit_journal::AuditInvocation {
            separate_auditor: None,
            schema_version: 1,
            task_id: f.base.id().into(),
            finishing_step_id: "step-1".into(),
            candidate_hash: "a".repeat(64),
            acquired_ledger_hash: "b".repeat(64),
            contract_hash: "c".repeat(64),
            payload_hash: "d".repeat(64),
            action_id: format!("{}:audit-1", f.base.id()),
            provider: f.prepared.profile().clone(),
            limits: f.prepared.limits().values(),
            pre_usage: task.detail.usage.clone(),
        };
        {
            let tx = f.base.f.store.conn.transaction().unwrap();
            super::super::semantic_audit_journal::insert(&tx, &record).unwrap();
            tx.commit().unwrap();
        }
        assert!(validate_invocation_journal(&f.base.f.store.conn, &task).is_err());
        let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        assert!(load_next_prompt_task(&tx, f.base.id()).is_err());
    }

    #[test]
    fn audit_enable_refuses_historical_role_without_writing() {
        let mut f = completion::FinishFixture::new(false);
        let task = f.saved();
        let before = encode(&task, MAX_RECORD_BYTES).unwrap();
        assert!(
            f.base
                .f
                .store
                .enable_next_prompt_semantic_audit(
                    &f.base.f.execution,
                    task.detail.summary.revision
                )
                .is_err()
        );
        assert_eq!(encode(&f.saved(), MAX_RECORD_BYTES).unwrap(), before);
    }

    // AC-0566: a recomputed mutable record hash cannot replace the saved action.
    #[test]
    fn audit_candidate_reconstructs_saved_finish_and_detects_changed_limitation() {
        let mut fixture = completion::FinishFixture::new(true);
        fixture.begin();
        fixture.publish().unwrap();
        let mut candidate = fixture
            .base
            .f
            .store
            .investigation_result(fixture.base.id())
            .unwrap()
            .unwrap();
        assert_eq!(
            candidate_action_hash(&candidate).unwrap(),
            fixture.response.action_hash
        );
        candidate.limitations[0] = "The evidence lacks successful handling.".into();
        // A new candidate hash does not change the immutable response witness.
        let forged_hash = core_prov::content_hash(&encode(&candidate, MAX_RESULT_BYTES).unwrap());
        assert!(!forged_hash.is_empty());
        assert_ne!(
            candidate_action_hash(&candidate).unwrap(),
            fixture.response.action_hash
        );
    }
}
