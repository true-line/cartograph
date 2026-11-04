//! Internal complete-response admission. Results and refusal decisions are
//! derived from the saved invocation and genuine supplied input, not caller DTOs.
use super::*;
use llm::bounded::PreparedBoundedCompletion;
use serde_json::Value;

pub(crate) struct NextPromptFinish<'a> {
    pub request: &'a StagedObligationRequest,
    pub response: &'a InvestigationResponseMeta,
    /// Canonical original envelope, including any replay_query identity.
    pub original_action: &'a Value,
    pub current: &'a InvestigationInput,
    pub current_prepared: &'a PreparedBoundedCompletion,
}

impl JobStore {
    pub(crate) fn admit_next_prompt_finish(
        &mut self,
        execution: &JobExecution,
        expected_revision: u64,
        input: &NextPromptFinish<'_>,
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
        if task.detail.summary.revision != expected_revision || task.phase != Phase::Invocation {
            return Err(InvestigationStoreError::Stale);
        }
        // A valid finish belongs to its supplied input even if cancellation won
        // while the provider response was in flight. Refusals retain the normal
        // cancellation precedence and can never start a new acquisition.
        transitions::check_job(&tx, execution, true)?;
        let effective_policy = super::policy_for_detail(&task.detail)?;
        let policy = &effective_policy;
        let request = input.request.normalized_with_policy(policy)?;
        if request.intent_hash_with_policy(policy)? != task.intent_hash {
            return Err(InvestigationStoreError::Conflict);
        }
        let authenticated = metadata_admission::authenticate_saved_response(
            &tx,
            &task,
            input.response,
            input.original_action,
            input.current,
            input.current_prepared,
            input.response.active_milliseconds,
        )?;
        let context = authenticated
            .core
            .ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        let action = ProgressiveInvestigationAction::decode_resolved_for_specialist(
            std::str::from_utf8(&authenticated.original)
                .map_err(|_| InvestigationStoreError::Invalid)?,
            &context.context_id,
            &task.intent_hash,
            &authenticated.core.ledger,
            policy,
            task.detail.summary.specialist_id,
        )?;
        match action {
            CompletionInvestigationAction::Read(_) => {
                if authenticated.call.mode() != CompletionCallMode::FinishOnly {
                    return Err(InvestigationStoreError::Invalid);
                }
                persist_response(&tx, &mut task, input.response, authenticated.admitted_usage)?;
                // The complete response is measured. Do not use the generic
                // unknown-invocation stop path, which clears reported usage.
                task.phase = Phase::Terminal;
                task.detail.summary.status = if task.detail.summary.cancel_requested {
                    InvestigationStatus::Cancelled
                } else {
                    InvestigationStatus::Failed
                };
                task.detail.error = Some(
                    InvestigationStopReason::FinishOnlyToolProposal
                        .message()
                        .into(),
                );
                bump(&tx, &mut task)?;
                let kind = if task.detail.summary.cancel_requested {
                    InvestigationEventKind::Cancelled
                } else {
                    InvestigationEventKind::Failed
                };
                event(
                    &tx,
                    &mut task,
                    kind,
                    None,
                    None,
                    InvestigationStopReason::FinishOnlyToolProposal.message(),
                )?;
                refresh_observation(&tx, &mut task)?;
                transitions::sync_terminal_job(&tx, &task)?;
            }
            CompletionInvestigationAction::Finish(finish) => {
                let entries = attempts(&tx, &task)?;
                let ledgers = entries
                    .iter()
                    .map(|entry| ledger_at(&tx, &id, entry.record.input_revision))
                    .collect::<Result<Vec<_>, _>>()?;
                let evidence = entries
                    .iter()
                    .zip(&ledgers)
                    .map(|(entry, ledger)| AttemptEvidence {
                        record: &entry.record,
                        ledger,
                    })
                    .collect::<Vec<_>>();
                let disclosure = authenticated
                    .core
                    .step
                    .disclosure
                    .as_ref()
                    .ok_or(InvestigationStoreError::Invalid)?;
                let basis = admission_journal::AuthenticatedCompletionBasis::load(
                    &tx,
                    &task,
                    &authenticated.call.step_id,
                )?;
                if let Some(basis) = &basis {
                    basis.check_response(&authenticated)?;
                }
                let decision = if let Some(basis) = &basis {
                    basis.binding()?.admit_finish(
                        input.current,
                        &request,
                        &finish,
                        &evidence,
                        input.response.observed_model.clone(),
                        timestamp(&tx)?,
                    )
                } else {
                    input.current.admit_completion_finish_with_policy(
                        &request,
                        &id,
                        &authenticated.call.step_id,
                        disclosure,
                        &finish,
                        &evidence,
                        input.response.observed_model.clone(),
                        timestamp(&tx)?,
                        policy,
                    )
                };
                match decision {
                    Ok(Ok(result)) => {
                        validate_result_basis(
                            &result,
                            &authenticated.core.ledger,
                            &authenticated.core.step,
                            &task.detail.limits,
                            basis.as_ref(),
                        )?;
                        validate_inspection_result(&task, &result)?;
                        if task.semantic_audit.is_some()
                            && result.completion.as_ref().is_some_and(|completion| {
                                completion.outcome == CompletionOutcome::Supported
                            })
                        {
                            // Structural admission is not semantic acceptance.
                            // Save usage and candidate atomically without a result
                            // row, chat reference, or Completed publication.
                            persist_response(
                                &tx,
                                &mut task,
                                input.response,
                                authenticated.admitted_usage.clone(),
                            )?;
                            super::super::semantic_audit_state::hold_candidate(
                                &mut task,
                                result,
                                &authenticated.admitted_usage,
                            )?;
                            bump(&tx, &mut task)?;
                        } else {
                            accepted(
                                &tx,
                                &mut task,
                                &result,
                                &authenticated.core.ledger,
                                &authenticated.core.step,
                                basis.as_ref(),
                            )?;
                            let body = encode(&result, MAX_RESULT_BYTES)?;
                            let result_hash = core_prov::content_hash(&body);
                            one(tx.execute(
                                "INSERT INTO investigation_results VALUES (?1,?2,?3)",
                                params![id, body, result_hash],
                            )?)?;
                            super::chat::insert_result_reference(
                                &tx,
                                &id,
                                &result.result_id,
                                &result_hash,
                            )?;
                            persist_response(
                                &tx,
                                &mut task,
                                input.response,
                                authenticated.admitted_usage,
                            )?;
                            task.phase = Phase::Terminal;
                            task.detail.summary.status = InvestigationStatus::Completed;
                            task.detail.summary.has_result = true;
                            bump(&tx, &mut task)?;
                            event(
                                &tx,
                                &mut task,
                                InvestigationEventKind::ResultPersisted,
                                Some(&input.response.step_id),
                                None,
                                "Cited findings were persisted against the original input ledger.",
                            )?;
                            transitions::sync_terminal_job(&tx, &task)?;
                        }
                    }
                    Ok(Err(FinishDecision::Refused { .. })) => {
                        refused(
                            &tx,
                            &self.namespace,
                            execution,
                            &mut task,
                            &request,
                            input.current,
                            &finish,
                            input.response.clone(),
                            basis.as_ref(),
                        )?;
                    }
                    Ok(Err(FinishDecision::Accepted(_))) => {
                        return Err(InvestigationStoreError::Invalid);
                    }
                    Err(InvestigationError::InvalidCitation) => {
                        // The saved call and full response were authenticated above.
                        // Retain the measured response, never the rejected finding.
                        persist_response(
                            &tx,
                            &mut task,
                            input.response,
                            authenticated.admitted_usage,
                        )?;
                        invalid_citation(&tx, &mut task, &input.response.step_id)?;
                    }
                    Err(InvestigationError::SourceReplay)
                        if task.detail.summary.specialist_id == SpecialistId::DomainAnalystV20 =>
                    {
                        persist_response(
                            &tx,
                            &mut task,
                            input.response,
                            authenticated.admitted_usage,
                        )?;
                        finish_refused(
                            &tx,
                            &mut task,
                            &input.response.step_id,
                            InvestigationStopReason::InvalidFinish(
                                InvestigationError::SourceReplay,
                            )
                            .message(),
                        )?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        admission_journal::derive_current(&tx, &task)?;
        validate_saved(&tx, &task)?;
        save_next_prompt_task(&tx, &task, expected_revision, task.phase == Phase::Terminal)?;
        #[cfg(test)]
        if FAIL_BEFORE_FINISH_COMMIT.with(|flag| flag.replace(false)) {
            return Err(InvestigationStoreError::Capacity);
        }
        tx.commit()?;
        #[cfg(test)]
        if LOSE_FINISH_ACK.with(|flag| flag.replace(false)) {
            return Err(InvestigationStoreError::Storage);
        }
        Ok(task.detail)
    }
}

fn persist_response(
    tx: &Transaction<'_>,
    task: &mut StoredTask,
    response: &InvestigationResponseMeta,
    usage: InvestigationUsage,
) -> Result<(), InvestigationStoreError> {
    insert_body(
        tx,
        "investigation_responses",
        &task.detail.summary.investigation_id,
        &response.step_id,
        response,
        4096,
    )?;
    task.detail.usage = usage;
    task.pending = None;
    task.approved = false;
    task.action_tool = None;
    bump(tx, task)?;
    event(
        tx,
        task,
        InvestigationEventKind::ModelCompleted,
        Some(&response.step_id),
        None,
        "A complete provider response was admitted and its identity persisted.",
    )
}

fn invalid_citation(
    tx: &Transaction<'_>,
    task: &mut StoredTask,
    step_id: &str,
) -> Result<(), InvestigationStoreError> {
    const MESSAGE: &str =
        "The model's answer cited evidence it had not acquired. No findings were saved.";
    finish_refused(tx, task, step_id, MESSAGE)
}

fn finish_refused(
    tx: &Transaction<'_>,
    task: &mut StoredTask,
    step_id: &str,
    message: &'static str,
) -> Result<(), InvestigationStoreError> {
    task.phase = Phase::Terminal;
    task.detail.summary.status = if task.detail.summary.cancel_requested {
        InvestigationStatus::Cancelled
    } else {
        InvestigationStatus::Failed
    };
    task.detail.error = Some(message.into());
    bump(tx, task)?;
    event(
        tx,
        task,
        InvestigationEventKind::FinishRefused,
        Some(step_id),
        None,
        message,
    )?;
    let kind = if task.detail.summary.cancel_requested {
        InvestigationEventKind::Cancelled
    } else {
        InvestigationEventKind::Failed
    };
    event(tx, task, kind, Some(step_id), None, message)?;
    refresh_observation(tx, task)?;
    transitions::sync_terminal_job(tx, task)
}

#[cfg(test)]
thread_local! {
    static FAIL_BEFORE_FINISH_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LOSE_FINISH_ACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
#[path = "next_prompt_finish_tests.rs"]
mod tests;

#[cfg(test)]
pub(in crate::jobs::investigations) use tests::fixture::Fixture as FinishFixture;
