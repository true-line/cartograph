//! Owner-bound next-family worker preparation and single-attempt publication.
use super::*;
use crate::investigations::evidence::EvidenceGuards;
use crate::jobs::investigations::NextPromptInvocation;

impl<R: tauri::Runtime> Worker<'_, R> {
    pub(super) fn admit_next_finish_or_action_refusal(
        &mut self,
        request: &obligations::StagedObligationRequest,
        input: &InvestigationInput,
        prepared: &PreparedBoundedCompletion,
        completion: &BoundedCompletion,
        mode: staged_refusal::CompletionCallMode,
        step_id: &str,
    ) -> Result<bool, Failure> {
        let effective_policy = crate::jobs::investigations::policy_for_detail(&self.detail)?;
        let policy = &effective_policy;
        let context = input
            .ledger()
            .coverage
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        let held = self.pending_patch.take();
        let is_patch = held.is_some();
        // @29 (#2431): a patch the merge rejects goes back to the model as
        // invalid_action while the run's allowance lasts, and the holder stays
        // for the retry. Past the allowance the patch settles below, terminal.
        if let Some(holder) = &held
            && self.detail.summary.specialist_id.uses_native_tool_runtime()
            && let Some(rejection) = holder.rejection(
                &completion.text,
                holder.basis(),
                self.detail.summary.specialist_id.patches_whole_finish(),
            )
            && self.refuse_rejected_patch(completion, step_id, rejection)?
        {
            self.pending_patch = held;
            return Ok(true);
        }
        let action = if is_patch {
            None
        } else {
            match ProgressiveInvestigationAction::decode_resolved_detailed_for_specialist(
                &completion.text,
                &context.context_id,
                &request.intent_hash_with_policy(policy)?,
                input.ledger(),
                policy,
                self.detail.summary.specialist_id,
            ) {
                Ok(action) => Some(action),
                Err(failure) => {
                    return self.refuse_invalid_action(completion, mode, step_id, failure);
                }
            }
        };
        // This is a decoded acquisition that would consume a tool action.
        // Finish remains admissible at the tool cap; unrelated store failures
        // are never relabelled as this explicit exhausted-tool boundary.
        // An unoffered read is refused below without charging a tool action,
        // so the tool cap is not its cause.
        if self
            .detail
            .summary
            .specialist_id
            .uses_query_budget_refusals()
            && mode == staged_refusal::CompletionCallMode::Choice
            && let Some(CompletionInvestigationAction::Read(read)) = &action
            && input.permits_acquisition(read)
            && self.detail.usage.tool_actions >= self.detail.limits.tool_actions
        {
            self.check_new_work()?;
            return Err(Failure::Stop(InvestigationStopReason::BudgetCap(
                agents::investigation::QueryBudgetTermination::ToolActions,
            )));
        }
        let original: serde_json::Value = serde_json::from_str(&completion.text)
            .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
        let refusal = if let Some(CompletionInvestigationAction::Read(read)) = &action
            && mode == staged_refusal::CompletionCallMode::Choice
        {
            let reason = if !input.permits_acquisition(read) {
                ActionRefusalReason::ActionNotOffered
            } else if self.detail.summary.specialist_id.uses_acquisition_choices()
                // Strict decoding above authenticated the saved replay selector.
                // An explicitly requested replay is not an accidental raw repeat.
                && original["type"] != "replay_query"
                && read.repeats_acquired_metadata(input.ledger())
            {
                ActionRefusalReason::RepeatedRead
            } else {
                return Ok(false);
            };
            // Cancellation/deadlines precede refusal. Unlike a late finish,
            // an acquisition response cannot rebase to a newer task revision.
            let revision = self.detail.summary.revision;
            self.check_new_work()?;
            if self.detail.summary.revision != revision {
                return Err(InvestigationStoreError::Stale.into());
            }
            Some(reason)
        } else {
            None
        };
        let response = InvestigationResponseMeta {
            finish_feedback_witness: None,
            finish_telemetry: None,
            step_id: step_id.into(),
            response_hash: content_hash(completion.text.as_bytes()),
            action_hash: self.hold_action(bounded_json(&original, 32 * 1024)?),
            observed_model: completion.response_model.clone(),
            reported_input_tokens: completion
                .usage
                .as_ref()
                .and_then(|usage| usage.input_tokens),
            reported_output_tokens: completion
                .usage
                .as_ref()
                .and_then(|usage| usage.output_tokens),
            reported_cache_read_tokens: completion
                .usage
                .as_ref()
                .and_then(|usage| usage.cache_read_input_tokens),
            active_milliseconds: self.clock.active_milliseconds(),
        };
        if let Some(reason) = refusal {
            self.detail = self
                .state
                .jobs
                .lock()
                .map_err(|_| HostError::Operational)?
                .refuse_next_prompt(
                    self.execution,
                    self.detail.summary.revision,
                    &response,
                    &original,
                    crate::jobs::investigations::NextPromptRefusalReason::Action(reason),
                )?;
            super::super::emit_changed(self.app, &self.detail);
            return Ok(true);
        }
        // A response is admitted once against its authenticated saved call.
        // A lost revision or acknowledgement is not permission to resubmit it.
        // Read the current revision once so a cancellation during the call can
        // retain a valid late finish (AC-0187). The writer authenticates the
        // original saved call/input; this does not rebase an acquisition.
        let (detail, patch) = {
            let mut jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
            let current = jobs.investigation_owned_detail(self.execution, self.id)?;
            if terminal(current.summary.status) {
                return Err(Failure::AlreadyTerminal);
            }
            let basis = &self.detail.summary;
            let same_basis = current.summary.revision == basis.revision;
            let cancel_only = !basis.cancel_requested
                && current.summary.cancel_requested
                && basis.revision.checked_add(1) == Some(current.summary.revision)
                && basis.last_event_sequence.checked_add(1)
                    == Some(current.summary.last_event_sequence)
                && current.summary.status == basis.status
                && current.summary.invocation_pending
                && basis.invocation_pending
                && current.usage == self.detail.usage;
            if !same_basis && !cancel_only {
                return Err(InvestigationStoreError::Stale.into());
            }
            // The writer also requires Invocation and authenticates the exact
            // saved step/input, so the cancellation exception cannot rebind it.
            let detail = jobs.admit_next_prompt_finish_with_patch(
                self.execution,
                current.summary.revision,
                &crate::jobs::investigations::NextPromptFinish {
                    request,
                    response: &response,
                    original_action: &original,
                    current: input,
                    current_prepared: prepared,
                },
                held,
                is_patch.then_some(completion.text.as_str()),
                Some(&crate::jobs::investigations::RejectedCompletion {
                    step_id: step_id.into(),
                    text: completion.text.clone(),
                    action: None,
                }),
            )?;
            let patch = jobs.investigation_pending_patch(self.execution, self.id)?;
            (detail, patch)
        };
        if let Some(patch) = patch {
            let Some(CompletionInvestigationAction::Finish(finish)) = action else {
                return Err(HostError::InvalidInput.into());
            };
            self.pending_patch = Some(
                agents::investigation::finish_patch::VolatileFinish::capture(finish, patch)
                    .map_err(|_| HostError::InvalidInput)?,
            );
        }
        self.detail = detail;
        super::super::emit_changed(self.app, &self.detail);
        Ok(true)
    }

    /// @29 (AC-1420): a choice-mode call that does not decode is refused as
    /// `invalid_action` and goes back to the model, up to
    /// [`agents::investigation::MAX_INVALID_ACTION_REFUSALS`] times a run; the
    /// journal enforces the cap, and a call past it stops the run as before.
    fn refuse_invalid_action(
        &mut self,
        completion: &BoundedCompletion,
        mode: staged_refusal::CompletionCallMode,
        step_id: &str,
        failure: agents::investigation::ActionDecodeFailure,
    ) -> Result<bool, Failure> {
        let stop = |failure| Failure::Stop(InvestigationStopReason::ActionDecode(failure));
        if !self.detail.summary.specialist_id.uses_native_tool_runtime()
            || mode != staged_refusal::CompletionCallMode::Choice
        {
            return Err(stop(failure));
        }
        let Ok(original @ serde_json::Value::Object(_)) =
            serde_json::from_str::<serde_json::Value>(&completion.text)
        else {
            return Err(stop(failure));
        };
        let response = self.refused_response(completion, step_id, &original)?;
        let refused = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .refuse_next_prompt(
                self.execution,
                self.detail.summary.revision,
                &response,
                &original,
                crate::jobs::investigations::NextPromptRefusalReason::Action(
                    ActionRefusalReason::InvalidAction,
                ),
            );
        self.detail = match refused {
            Ok(detail) => detail,
            // The run already returned its allowance of undecodable calls.
            Err(InvestigationStoreError::Capacity) => return Err(stop(failure)),
            Err(error) => return Err(error.into()),
        };
        super::super::emit_changed(self.app, &self.detail);
        Ok(true)
    }

    /// @29 (#2431): journal a rejected finish patch as `invalid_action`. False
    /// when the response is not a JSON object or the run's allowance is spent;
    /// the caller then settles the patch, which rejects it terminally.
    fn refuse_rejected_patch(
        &mut self,
        completion: &BoundedCompletion,
        step_id: &str,
        rejection: agents::investigation::finish_patch::PatchRejection,
    ) -> Result<bool, Failure> {
        let Ok(original @ serde_json::Value::Object(_)) =
            serde_json::from_str::<serde_json::Value>(&completion.text)
        else {
            return Ok(false);
        };
        let response = self.refused_response(completion, step_id, &original)?;
        let refused = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .refuse_next_prompt_patch(
                self.execution,
                self.detail.summary.revision,
                &response,
                &original,
                rejection,
            );
        self.detail = match refused {
            Ok(detail) => detail,
            Err(InvestigationStoreError::Capacity) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        super::super::emit_changed(self.app, &self.detail);
        Ok(true)
    }

    /// The response record of a refused call. Cancellation and new work
    /// precede the refusal, which never rebases to a newer revision.
    fn refused_response(
        &mut self,
        completion: &BoundedCompletion,
        step_id: &str,
        original: &serde_json::Value,
    ) -> Result<InvestigationResponseMeta, Failure> {
        let revision = self.detail.summary.revision;
        self.check_new_work()?;
        if self.detail.summary.revision != revision {
            return Err(InvestigationStoreError::Stale.into());
        }
        Ok(InvestigationResponseMeta {
            finish_feedback_witness: None,
            finish_telemetry: None,
            step_id: step_id.into(),
            response_hash: content_hash(completion.text.as_bytes()),
            action_hash: self.hold_action(bounded_json(original, 32 * 1024)?),
            observed_model: completion.response_model.clone(),
            reported_input_tokens: completion
                .usage
                .as_ref()
                .and_then(|usage| usage.input_tokens),
            reported_output_tokens: completion
                .usage
                .as_ref()
                .and_then(|usage| usage.output_tokens),
            reported_cache_read_tokens: completion
                .usage
                .as_ref()
                .and_then(|usage| usage.cache_read_input_tokens),
            active_milliseconds: self.clock.active_milliseconds(),
        })
    }

    pub(super) fn publish_next_invocation(
        &mut self,
        input: &InvestigationInput,
        prepared: &PreparedBoundedCompletion,
        context: &FrozenContext,
        authorized: Option<&llm::bounded::AuthorizedBoundedCompletion>,
    ) -> Result<(), Failure> {
        if input.ledger() != &context.ledger {
            return Err(HostError::InputChanged.into());
        }
        let keys = context
            .ledger
            .selected_facts
            .iter()
            .map(|selected| crate::task_evidence::graph_fact(&selected.fact))
            .collect::<Vec<_>>();
        let selected = context.select(self.state, &keys)?;
        let guards = EvidenceGuards::acquire(self.state, &selected)?;
        // The Ready prompt sampled RunClock before construction; Consent keeps
        // that exact sealed stamp throughout human waiting and approval.
        let active_milliseconds = input
            .seal_disclosure_with_policy(prepared, &context.policy)?
            .and_then(|disclosure| disclosure.admission)
            .ok_or(HostError::InvalidInput)?
            .consumed_budgets
            .active_milliseconds;
        let invocation = NextPromptInvocation {
            input,
            prepared,
            active_milliseconds,
        };
        let mut retained_error = None;
        let validate_retained = |view: &crate::sources::RetainedSourceView<'_>| {
            let checked = self
                .clock
                .deadline()
                .map(|_| ())
                .and_then(|()| guards.revalidate_in_view(&self.state.primary_sources, view));
            if let Err(error) = checked {
                retained_error = Some(error);
                return Err(InvestigationStoreError::Invalid);
            }
            Ok(())
        };
        let result = {
            let mut jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
            if let Some(authorized) = authorized {
                jobs.begin_next_prompt_invocation(
                    self.execution,
                    self.detail.summary.revision,
                    &invocation,
                    authorized,
                    validate_retained,
                )
            } else {
                jobs.await_next_prompt_consent(
                    self.execution,
                    self.detail.summary.revision,
                    &invocation,
                    validate_retained,
                )
            }
        };
        if let Some(error) = retained_error {
            return Err(error.into());
        }
        self.detail = result?;
        Ok(())
    }

    pub(super) fn publish_initial_next_prompt(
        &mut self,
        request: &obligations::StagedObligationRequest,
        provider: &dyn LlmProvider,
        context: &FrozenContext,
        history: &[String],
    ) -> Result<(), Failure> {
        let (detail, metadata, initial_contract) = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .investigation_next_prompt_initial_state(
                self.execution,
                self.detail.summary.revision,
                request,
            )?;
        let effective_policy = crate::jobs::investigations::policy_for_detail(&self.detail)?;
        let policy = &effective_policy;
        let mut usage = detail.usage.clone();
        usage.active_milliseconds = self.clock.active_milliseconds();
        let query_pages = context
            .query_pages
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let inventory_details = context
            .inventory_details
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let input = InvestigationInput::prepare_v9_withholding_with_policy(
            &request.question,
            detail.summary.specialist_id,
            context.ledger.clone(),
            &query_pages,
            &context.supplied_evidence(),
            history,
            &usage,
            &InvestigationTurnBasis {
                input_ledger_hash: context.ledger.fingerprint_with_policy(policy)?,
                previous_ledger: None,
                completed_tool: None,
            },
            request,
            &metadata,
            &inventory_details,
            staged_refusal::CompletionCallMode::Choice,
            0,
            None,
            policy,
            &context.withheld,
        )?;
        let input = match initial_contract {
            Some(contract) => {
                let choices = acquisition_choices::AcquisitionChoices::derive(
                    &contract,
                    &metadata,
                    self.id,
                    &context.ledger,
                    &[],
                    policy,
                )?;
                // This producer is not publication authority: the initial writer
                // independently rederives the stamp on its staged ledger/archive.
                input.with_acquisition_admission(Arc::new(choices), None)?
            }
            None => input,
        }; // construction-only contract is dropped before firewall/sealing
        let action = input.action(format!("{}:step-1", self.id))?;
        let limits = self.completion_limits(policy, &input)?;
        let prepared = firewall(self.state)?
            .prepare_bounded(provider, &action, &limits)
            .map_err(provider_failure)?;
        let archive = context
            .coverage_archive
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        let keys = context
            .ledger
            .selected_facts
            .iter()
            .map(|selected| crate::task_evidence::graph_fact(&selected.fact))
            .collect::<Vec<_>>();
        let selected = context.select(self.state, &keys)?;
        let guards = EvidenceGuards::acquire(self.state, &selected)?;
        let mut retained_error = None;
        let result = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .admit_next_prompt_initial_context(
                self.execution,
                detail.summary.revision,
                &NextPromptInvocation {
                    input: &input,
                    prepared: &prepared,
                    active_milliseconds: usage.active_milliseconds,
                },
                archive,
                |view| {
                    let checked = self.clock.deadline().map(|_| ()).and_then(|()| {
                        guards.revalidate_in_view(&self.state.primary_sources, view)
                    });
                    if let Err(error) = checked {
                        retained_error = Some(error);
                        return Err(InvestigationStoreError::Invalid);
                    }
                    Ok(())
                },
            );
        if let Some(error) = retained_error {
            return Err(error.into());
        }
        self.detail = result?;
        self.record_scope_size(context);
        super::super::emit_changed(self.app, &self.detail);
        Ok(())
    }
}
