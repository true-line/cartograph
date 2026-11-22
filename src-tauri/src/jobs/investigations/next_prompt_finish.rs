//! Internal complete-response admission. Results and refusal decisions are
//! derived from the saved invocation and genuine supplied input, not caller DTOs.
use super::*;
use llm::bounded::PreparedBoundedCompletion;
use serde_json::Value;

#[path = "withholding_finish.rs"]
mod withholding_finish;

pub struct NextPromptFinish<'a> {
    pub request: &'a StagedObligationRequest,
    pub response: &'a InvestigationResponseMeta,
    /// Canonical original envelope, including any replay_query identity.
    pub original_action: &'a Value,
    pub current: &'a InvestigationInput,
    pub current_prepared: &'a PreparedBoundedCompletion,
}

impl JobStore {
    #[cfg(any(test, feature = "test-support"))]
    pub fn admit_next_prompt_finish(
        &mut self,
        execution: &JobExecution,
        expected_revision: u64,
        input: &NextPromptFinish<'_>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.admit_next_prompt_finish_with_patch(
            execution,
            expected_revision,
            input,
            None,
            None,
            None,
        )
    }

    pub fn admit_next_prompt_finish_with_patch(
        &mut self,
        execution: &JobExecution,
        expected_revision: u64,
        input: &NextPromptFinish<'_>,
        held: Option<agents::investigation::finish_patch::VolatileFinish>,
        raw_patch: Option<&str>,
        rejected: Option<&super::RejectedCompletion>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        execution.verify()?;
        if input.response.finish_feedback_witness.is_some()
            || input.response.finish_telemetry.is_some()
        {
            return Err(InvestigationStoreError::Invalid);
        }
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
        let merged = match (task.pending_patch(), held, raw_patch) {
            (Some(_), Some(held), Some(raw)) => Some(super::super::volatile_patch::merge(
                &tx,
                &task,
                &authenticated,
                input.response,
                raw,
                held,
            )?),
            (None, None, None) => None,
            _ => return Err(InvestigationStoreError::Invalid),
        };
        let merged = match merged {
            Some(Ok(value)) => Some(value),
            Some(Err(code)) => {
                let mut response = input.response.clone();
                response.finish_telemetry = Some(
                    super::super::volatile_patch::FinishTelemetry::rejected(code),
                );
                persist_response(&tx, &mut task, &response, authenticated.admitted_usage)?;
                task.pending_correction = None;
                task.phase = Phase::Terminal;
                task.detail.summary.status = InvestigationStatus::Failed;
                task.detail.error =
                    Some("The patch correction was rejected. No findings were saved.".into());
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::Failed,
                    Some(&response.step_id),
                    None,
                    "The authenticated patch response failed deterministic projection.",
                )?;
                refresh_observation(&tx, &mut task)?;
                transitions::sync_terminal_job(&tx, &task)?;
                save_next_prompt_task(&tx, &task, expected_revision, true)?;
                tx.commit()?;
                return Ok(task.detail);
            }
            None => None,
        };
        let patch_counts = merged.as_ref().map(|(merged, _)| merged.counts.clone());
        let (action, merge_authority, patch_eligible) = if let Some((merged, authority)) = merged {
            (
                CompletionInvestigationAction::Finish(merged.finish),
                Some(authority),
                Some(merged.flagged_merged_indices),
            )
        } else {
            (
                ProgressiveInvestigationAction::decode_resolved_for_specialist(
                    std::str::from_utf8(&authenticated.original)
                        .map_err(|_| InvestigationStoreError::Invalid)?,
                    &context.context_id,
                    &task.intent_hash,
                    &authenticated.core.ledger,
                    policy,
                    task.detail.summary.specialist_id,
                )?,
                None,
                None,
            )
        };
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
                let decision = match decision {
                    Ok(Err(FinishDecision::Refused {
                        ref feedback,
                        ref assessment,
                    })) if feedback.unmet.is_empty()
                        && assessment.iter().all(|item| item.unmet.is_none()) =>
                    {
                        if let Some(basis) = &basis {
                            match withholding_finish::project(
                                &tx,
                                &task,
                                input.current,
                                &request,
                                &finish,
                                &evidence,
                                input.response,
                                basis,
                                policy,
                                patch_eligible.as_deref(),
                            )? {
                                Some(result) => Ok(Ok(result)),
                                None => decision,
                            }
                        } else {
                            decision
                        }
                    }
                    other => other,
                };
                // A spent allowance keeps the original terminal replay path and accounting.
                let decision = match decision {
                    Ok(Err(FinishDecision::Refused { ref feedback, .. }))
                        if matches!(feedback.prose_correction, Some(agents::investigation::staged_finish::ProseCorrection::ParaphraseWithoutQuotingSource | agents::investigation::staged_finish::ProseCorrection::ParaphraseReplayAt(_)))
                            && task.detail.completion.as_ref().is_some_and(|c| c.refusal_count > 0) =>
                    {
                        Err(InvestigationError::SourceReplay)
                    }
                    other => other,
                };
                let mut measured_response = input.response.clone();
                if task.detail.summary.specialist_id.uses_finish_patch() {
                    let count = match &finish.action {
                        InvestigationAction::Finish { findings, .. } => findings.len(),
                        _ => return Err(InvestigationStoreError::Invalid),
                    };
                    let mut telemetry = super::super::volatile_patch::FinishTelemetry::finish(
                        count,
                        patch_counts.as_ref(),
                    );
                    match &decision {
                        Ok(Err(FinishDecision::Refused { feedback, .. })) => {
                            telemetry.refusal(feedback)
                        }
                        Ok(Ok(result)) => {
                            telemetry.flagged_count = Some(
                                result
                                    .completion
                                    .as_ref()
                                    .and_then(|c| c.withholding.as_ref())
                                    .map_or(0, |w| w.withheld_indexes.len()),
                            );
                        }
                        Err(_) => {
                            telemetry.flagged_count = None;
                            telemetry.projection_rejection = Some(agents::investigation::finish_patch::PatchRejection::AdmissionRejected);
                        }
                        _ => {}
                    }
                    measured_response.finish_telemetry = Some(telemetry);
                }
                if merge_authority.is_some()
                    && let Ok(Ok(result)) = &decision
                    && result
                        .completion
                        .as_ref()
                        .is_some_and(|c| c.withholding.is_none())
                {
                    measured_response.finish_feedback_witness =
                        Some(super::super::volatile_patch::result_witness(
                            &task,
                            &measured_response,
                            result,
                            basis.as_ref().ok_or(InvestigationStoreError::Invalid)?,
                        )?);
                }
                let measured_input = NextPromptFinish {
                    request: input.request,
                    response: &measured_response,
                    original_action: input.original_action,
                    current: input.current,
                    current_prepared: input.current_prepared,
                };
                let input = &measured_input;
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
                                matches!(
                                    completion.outcome,
                                    CompletionOutcome::Supported
                                        | CompletionOutcome::FindingsWithheldAfterCorrection
                                )
                            })
                        {
                            // Structural admission is not semantic acceptance.
                            // Save usage and candidate atomically without a result
                            // row, chat reference, or Completed publication.
                            let mut response = input.response.clone();
                            if let Some(witness) = result
                                .completion
                                .as_ref()
                                .and_then(|c| c.withholding.as_ref())
                            {
                                response.finish_feedback_witness =
                                    Some(refusal_diagnostics::withholding_witness(
                                        &tx,
                                        &task.detail.summary.investigation_id,
                                        witness.fingerprint()?,
                                    )?);
                            }
                            persist_response(
                                &tx,
                                &mut task,
                                &response,
                                authenticated.admitted_usage.clone(),
                            )?;
                            super::super::semantic_audit_state::hold_candidate(
                                &mut task,
                                result,
                                &authenticated.admitted_usage,
                            )?;
                            bump(&tx, &mut task)?;
                        } else {
                            let partial = result
                                .completion
                                .as_ref()
                                .and_then(|c| c.withholding.as_ref());
                            if partial.is_some() || merge_authority.is_some() {
                                let mut response = input.response.clone();
                                if let Some(witness) = partial {
                                    response.finish_feedback_witness =
                                        Some(refusal_diagnostics::withholding_witness(
                                            &tx,
                                            &task.detail.summary.investigation_id,
                                            witness.fingerprint()?,
                                        )?);
                                }
                                // Insert independent response metadata before validating the
                                // result; all writes roll back together if any check fails.
                                persist_response(
                                    &tx,
                                    &mut task,
                                    &response,
                                    authenticated.admitted_usage.clone(),
                                )?;
                            }
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
                            if partial.is_none() && merge_authority.is_none() {
                                persist_response(
                                    &tx,
                                    &mut task,
                                    input.response,
                                    authenticated.admitted_usage,
                                )?;
                            }
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
                            merge_authority.as_ref(),
                        )?;
                    }
                    Ok(Err(FinishDecision::Accepted(_))) => {
                        return Err(InvestigationStoreError::Invalid);
                    }
                    Err(error) => {
                        // The response identity was authenticated above. Rejected
                        // prose is never persisted; hashes and usage are safe to retain.
                        persist_response(
                            &tx,
                            &mut task,
                            input.response,
                            authenticated.admitted_usage,
                        )?;
                        task.detail.terminal_diagnostic = Some(
                            agents::investigation::InvestigationTerminalDiagnostic::RejectedFinish {
                                reason: error.into(),
                            },
                        );
                        // AC-1404 (ADR-0076): keep the refused response's
                        // bounded redacted record; never decisive. Only the
                        // response this admission authenticated is kept.
                        if let Some(rejected) = rejected
                            && rejected.step_id == input.response.step_id
                            && core_prov::content_hash(rejected.text.as_bytes())
                                == input.response.response_hash
                        {
                            super::rejected_responses::record_rejected(&tx, &task, rejected)?;
                        }
                        if error == InvestigationError::InvalidCitation {
                            invalid_citation(&tx, &mut task, &input.response.step_id)?;
                        } else {
                            finish_refused(
                                &tx,
                                &mut task,
                                &input.response.step_id,
                                InvestigationStopReason::InvalidFinish(error).message(),
                            )?;
                        }
                    }
                }
            }
        }
        if task.phase == Phase::Terminal {
            task.pending_correction = None;
        }
        admission_journal::derive_current(&tx, &task)?;
        validate_saved(&tx, &task)?;
        save_next_prompt_task(&tx, &task, expected_revision, task.phase == Phase::Terminal)?;
        #[cfg(any(test, feature = "test-support"))]
        if FAIL_BEFORE_FINISH_COMMIT.with(|flag| flag.replace(false)) {
            return Err(InvestigationStoreError::Capacity);
        }
        tx.commit()?;
        #[cfg(any(test, feature = "test-support"))]
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

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_BEFORE_FINISH_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LOSE_FINISH_ACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-support"))]
#[path = "next_prompt_finish_tests.rs"]
mod tests;

#[cfg(any(test, feature = "test-support"))]
pub(in crate::jobs::investigations) use tests::fixture::Fixture as FinishFixture;
