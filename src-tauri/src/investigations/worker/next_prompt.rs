//! Owner-bound next-family worker preparation and single-attempt publication.
use super::*;
use crate::investigations::evidence::EvidenceGuards;
use crate::jobs::investigations::NextPromptInvocation;

impl<R: tauri::Runtime> Worker<'_, R> {
    pub(super) fn admit_next_finish_response(
        &mut self,
        request: &obligations::StagedObligationRequest,
        input: &InvestigationInput,
        prepared: &PreparedBoundedCompletion,
        completion: &BoundedCompletion,
        mode: staged_refusal::CompletionCallMode,
        step_id: &str,
    ) -> Result<bool, Failure> {
        let policy = &INVESTIGATION_V2_POLICY;
        let context = input
            .ledger()
            .coverage
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        let action = ProgressiveInvestigationAction::decode_resolved(
            &completion.text,
            &context.context_id,
            &request.intent_hash_with_policy(policy)?,
            input.ledger(),
            policy,
        )
        .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
        if matches!(action, CompletionInvestigationAction::Read(_))
            && mode != staged_refusal::CompletionCallMode::FinishOnly
        {
            return Ok(false);
        }
        let original: serde_json::Value = serde_json::from_str(&completion.text)
            .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
        let response = InvestigationResponseMeta {
            step_id: step_id.into(),
            response_hash: content_hash(completion.text.as_bytes()),
            action_hash: content_hash(&bounded_json(&original, 32 * 1024)?),
            observed_model: completion.response_model.clone(),
            reported_input_tokens: completion
                .usage
                .as_ref()
                .and_then(|usage| usage.input_tokens),
            reported_output_tokens: completion
                .usage
                .as_ref()
                .and_then(|usage| usage.output_tokens),
            active_milliseconds: self.clock.active_milliseconds(),
        };
        // A response is admitted once against its authenticated saved call.
        // A lost revision or acknowledgement is not permission to resubmit it.
        // Read the current revision once so a cancellation during the call can
        // retain a valid late finish (AC-0187). The writer authenticates the
        // original saved call/input; this does not rebase an acquisition.
        let detail = {
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
            jobs.admit_next_prompt_finish(
                self.execution,
                current.summary.revision,
                &crate::jobs::investigations::NextPromptFinish {
                    request,
                    response: &response,
                    original_action: &original,
                    current: input,
                    current_prepared: prepared,
                },
            )?
        };
        self.detail = detail;
        super::super::emit_changed(self.app, &self.detail);
        Ok(true)
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
            .seal_disclosure_with_policy(prepared, &INVESTIGATION_V2_POLICY)?
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
        let (detail, metadata) = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .investigation_next_prompt_initial_state(
                self.execution,
                self.detail.summary.revision,
                request,
            )?;
        let policy = &INVESTIGATION_V2_POLICY;
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
        let input = InvestigationInput::prepare_v9_with_policy(
            &request.question,
            detail.summary.specialist_id,
            context.ledger.clone(),
            &query_pages,
            &context.evidence,
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
        )?;
        let action = input.action(format!("{}:step-1", self.id))?;
        let mut transport = CompletionLimits::default().values();
        transport.max_output_tokens = policy.limits().generated_tokens_per_invocation;
        transport.max_spans =
            u32::try_from(policy.limits().evidence_items).map_err(|_| HostError::InvalidInput)?;
        let limits = CompletionLimits::new(transport).map_err(provider_failure)?;
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
        super::super::emit_changed(self.app, &self.detail);
        Ok(())
    }
}
