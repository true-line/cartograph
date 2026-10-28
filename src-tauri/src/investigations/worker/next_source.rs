//! Actual source reads cross reservation, durable validation, access and settlement.
use super::*;
use crate::investigations::evidence::prepared_source::{
    SourceReadFailureKind, SourceReservationError,
    publication::{
        SourcePublicationInput,
        cleanup::{SourceCleanupError, SourceCleanupFailure},
    },
};
use crate::jobs::investigations::SourceAdmission;

impl<R: tauri::Runtime> Worker<'_, R> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn admit_next_source_response(
        &mut self,
        request: &obligations::StagedObligationRequest,
        current: &InvestigationInput,
        current_prepared: &PreparedBoundedCompletion,
        completion: &BoundedCompletion,
        step_id: &str,
        provider: &dyn LlmProvider,
        context: &mut FrozenContext,
        history: &[String],
    ) -> Result<(), Failure> {
        let policy = &INVESTIGATION_V2_POLICY;
        let coverage = current
            .ledger()
            .coverage
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        let action = ProgressiveInvestigationAction::decode_resolved_detailed(
            &completion.text,
            &coverage.context_id,
            &request.intent_hash_with_policy(policy)?,
            current.ledger(),
            policy,
        )
        .map_err(|failure| Failure::Stop(InvestigationStopReason::ActionDecode(failure)))?;
        let CompletionInvestigationAction::Read(QualifiedInvestigationAction::SourceOrFinish(
            InvestigationAction::ReadEvidence { fact, role, index },
        )) = action
        else {
            return Err(Failure::Stop(InvestigationStopReason::InvalidResponse));
        };
        current
            .validate_admitted_source_fact(&fact)
            .map_err(|failure| Failure::Stop(InvestigationStopReason::ActionDecode(failure)))?;
        self.check_new_work()?;
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
            reported_cache_read_tokens: completion
                .usage
                .as_ref()
                .and_then(|usage| usage.cache_read_input_tokens),
            active_milliseconds: self.clock.active_milliseconds(),
        };
        // This uses frozen receipt metadata only; no captured text is read here.
        let source = context
            .prepare_captured_read(self.state, &fact, role, index)
            .map_err(|error| Failure::from(error).preparation_at(PreparationPhase::SourcePrepare))?
            .with_read_limits(self.task_limits.map(|limits| limits.evidence))
            .map_err(|error| {
                Failure::from(error).preparation_at(PreparationPhase::SourcePrepare)
            })?;
        let next_ordinal = self
            .detail
            .usage
            .model_invocations
            .checked_add(1)
            .ok_or(HostError::LimitExceeded)?;
        let next_action_id = format!("{}:step-{next_ordinal}", self.id);
        let proof = match current.certify_unread_source_v9_with_policy(
            current_prepared,
            source.target(),
            current_prepared.profile(),
            current_prepared.limits(),
            &next_action_id,
            policy,
        ) {
            Ok(proof) => proof,
            Err(InvestigationError::LimitExceeded) => {
                self.detail = self
                    .state
                    .jobs
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .refuse_next_prompt_capacity(
                        self.execution,
                        self.detail.summary.revision,
                        &response,
                        &original,
                        CapacityRefusalReason::PromptCapacity,
                    )?;
                super::super::emit_changed(self.app, &self.detail);
                return Ok(());
            }
            Err(error) => {
                return Err(
                    Failure::from(error).preparation_at(PreparationPhase::SourceCertification)
                );
            }
        };
        let firewall = firewall(self.state)?;
        let reservation = source
            .reserve(
                context,
                self.state,
                self.execution,
                self.detail.summary.revision,
                self.clock,
                &SourceAdmission {
                    response: &response,
                    original_action: &original,
                    current,
                    current_prepared,
                    proof: &proof,
                    target: source.target(),
                    active_milliseconds: self.clock.active_milliseconds(),
                },
            )
            .map_err(|error| match error {
                SourceReservationError::Host(error) => {
                    Failure::Host(error).preparation_at(PreparationPhase::SourceReservation)
                }
                SourceReservationError::Store(error) => Failure::Store(error),
            })?;
        self.detail = reservation.detail().clone();
        let publication = SourcePublicationInput {
            request,
            current,
            current_prepared,
            proof: &proof,
            supplied_history: history,
            firewall: &firewall,
            provider,
        };
        let result =
            match source.read_reserved(context, self.state, self.execution, &reservation.basis()) {
                Ok(candidate) => candidate.publish_acquired(
                    context,
                    self.state,
                    self.execution,
                    self.clock,
                    &publication,
                ),
                Err(failure) if failure.kind() == SourceReadFailureKind::ReadUnavailable => failure
                    .publish_unavailable(
                        context,
                        self.state,
                        self.execution,
                        self.clock,
                        &publication,
                    ),
                Err(failure) => {
                    self.detail = failure
                        .settle_terminal(self.state, self.execution, &reservation, self.clock)
                        .map_err(cleanup_failure)?;
                    super::super::emit_changed(self.app, &self.detail);
                    return Ok(());
                }
            };
        self.detail = match result {
            Ok(published) => published.detail,
            Err(failure) => failure
                .settle_terminal(self.state, self.execution, &reservation, self.clock)
                .map_err(cleanup_failure)?,
        };
        super::super::emit_changed(self.app, &self.detail);
        Ok(())
    }
}
fn cleanup_failure(failure: SourceCleanupFailure) -> Failure {
    // Never retry the read or publication after an uncertain acknowledgement.
    match failure.error() {
        SourceCleanupError::Host(error) => Failure::Host(*error),
        SourceCleanupError::Store(error) => Failure::Store(*error),
    }
}
