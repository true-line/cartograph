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
        let effective_policy = crate::jobs::investigations::policy_for_detail(&self.detail)?;
        let policy = &effective_policy;
        let coverage = current
            .ledger()
            .coverage
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        let action = ProgressiveInvestigationAction::decode_resolved_detailed_for_specialist(
            &completion.text,
            &coverage.context_id,
            &request.intent_hash_with_policy(policy)?,
            current.ledger(),
            policy,
            self.detail.summary.specialist_id,
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
            finish_feedback_witness: None,
            finish_telemetry: None,
            step_id: step_id.into(),
            response_hash: content_hash(completion.text.as_bytes()),
            action_hash: self.hold_action(source_response_action_bytes(&original)?),
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
        let sizes = if self
            .detail
            .summary
            .specialist_id
            .uses_exact_source_reservation()
        {
            source
                .size_binding()
                .sizes_for(source.target(), current.ledger())?
        } else {
            None
        };
        let certification = if let Some(sizes) = sizes {
            current.certify_measured_unread_source_v9_with_policy(
                current_prepared,
                source.target(),
                current_prepared.profile(),
                current_prepared.limits(),
                &next_action_id,
                policy,
                sizes,
            )
        } else {
            current.certify_unread_source_v9_with_policy(
                current_prepared,
                source.target(),
                current_prepared.profile(),
                current_prepared.limits(),
                &next_action_id,
                policy,
            )
        };
        // #1653 (AC-1818): opt-in operator trace of the read's continuation
        // fit, numeric only. No identifiers, paths, source or response text.
        if std::env::var_os("CARTOGRAPH_CAPACITY_TRACE").is_some() {
            eprintln!(
                "{}",
                capacity_trace_line(
                    next_ordinal,
                    serde_json::to_vec(&current_prepared.preview().egress.payload)
                        .map_or(0, |payload| payload.len() as u64),
                    source.target().source.byte_end - source.target().source.byte_start,
                    current_prepared.limits().values().input_bytes,
                    certification
                        .as_ref()
                        .ok()
                        .map(|proof| proof.continuation()),
                )
            );
        }
        let proof = match certification {
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
        // Opt-in test instrumentation: numeric call/byte counts only. No
        // identifiers, paths, source text, response text or refused prose.
        #[cfg(test)]
        if std::env::var_os("CARTOGRAPH_OFFLINE_REPRO").is_some() {
            let measured = proof.continuation();
            eprintln!(
                "auditor14_source_bound call={} choice={:?} finish_only={:?} input_allowed={} headroom={} disclosure_required={}",
                self.detail.usage.model_invocations,
                measured.choice,
                measured.finish_only,
                current_prepared.limits().values().input_bytes,
                current_prepared
                    .limits()
                    .values()
                    .input_bytes
                    .saturating_sub(
                        measured
                            .choice
                            .original_payload_bytes
                            .max(measured.choice.redacted_payload_bytes)
                    ),
                proof.disclosure_upper_bytes()
            );
        }
        let firewall = firewall(self.state)?;
        let reservation = source
            .reserve(
                context,
                self.state,
                self.execution,
                self.detail.summary.revision,
                self.clock,
                &SourceAdmission {
                    size_binding: (self
                        .detail
                        .summary
                        .specialist_id
                        .uses_exact_source_reservation())
                    .then(|| source.size_binding()),
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
                SourceReservationError::Store(error) => {
                    Failure::Store(error).preparation_at(PreparationPhase::SourceReservation)
                }
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

fn source_response_action_bytes(original: &serde_json::Value) -> Result<Vec<u8>, Failure> {
    Ok(bounded_json(original, 32 * 1024)?)
}

#[cfg(test)]
mod capacity_hash_tests {
    use super::*;
    use agents::investigation::acquisition_choices::AcquisitionAction;

    #[test]
    fn run10_model_response_and_capacity_menu_match_saved_action_hash() {
        // AC-0287 / #1689: independent run-10 original and saved hash, not
        // a response synthesized by serializing the offered menu entry.
        let original: serde_json::Value = serde_json::from_str(r#"{
            "type":"read_evidence", "role":"provenance", "index":0,
            "fact":{"kind":"node","id":"sym:local/src_95d3545ed92d87f7f0bf82ad9f31fe26@packages/core/src/config/catalog/multi-channel-stock-location-strategy.ts#MultiChannelStockLocationStrategy.forAllocation"}
        }"#).unwrap();
        let expected = "d92e64c2baec5e69580bfc68b4a956f006148b6e93856c44c8280f17c00c092c";
        let offered = AcquisitionAction::ReadEvidence {
            fact: agents::TaskFactKey::Node { id: "sym:local/src_95d3545ed92d87f7f0bf82ad9f31fe26@packages/core/src/config/catalog/multi-channel-stock-location-strategy.ts#MultiChannelStockLocationStrategy.forAllocation".into() },
            role: agents::TaskRangeRole::Provenance,
            index: 0,
        };
        assert_eq!(
            content_hash(&source_response_action_bytes(&original).unwrap()),
            expected
        );
        assert_eq!(offered.capacity_refusal_hash().unwrap(), expected);
        let mut shifted = offered.clone();
        if let AcquisitionAction::ReadEvidence { index, .. } = &mut shifted {
            *index = 1;
        }
        assert_ne!(shifted.capacity_refusal_hash().unwrap(), expected);
    }
}

/// One `capacity_trace` line: the call ordinal, the current payload, the source
/// range and the input ceiling, then the certified continuation sizes and the
/// headroom left, or `refused` when the continuation could not be proven.
fn capacity_trace_line(
    call: u32,
    current_payload: u64,
    source_bytes: u64,
    ceiling: u64,
    continuation: Option<agents::investigation::AdmissionReserveMeasurements>,
) -> String {
    let base = format!(
        "capacity_trace kind=read_evidence call={call} current_payload={current_payload} source_bytes={source_bytes} ceiling={ceiling}"
    );
    match continuation {
        Some(measured) => {
            let peak = [measured.choice, measured.finish_only]
                .iter()
                .map(|m| m.original_payload_bytes.max(m.redacted_payload_bytes))
                .max()
                .unwrap_or(0);
            format!(
                "{base} choice={} finish_only={} reserve={} headroom={}",
                measured
                    .choice
                    .original_payload_bytes
                    .max(measured.choice.redacted_payload_bytes),
                measured
                    .finish_only
                    .original_payload_bytes
                    .max(measured.finish_only.redacted_payload_bytes),
                peak.saturating_sub(current_payload),
                ceiling.saturating_sub(peak)
            )
        }
        None => format!("{base} refused"),
    }
}

#[cfg(test)]
mod capacity_trace_tests {
    use super::*;

    fn wire(bytes: u64) -> llm::bounded::BoundedWireMeasurements {
        llm::bounded::BoundedWireMeasurements {
            raw_string_bytes: bytes,
            original_payload_bytes: bytes,
            redacted_payload_bytes: bytes,
            original_action_bytes: bytes,
            egress_fingerprint_bytes: bytes,
        }
    }

    #[test]
    fn capacity_trace_names_reserve_and_headroom_or_refusal() {
        let admitted = capacity_trace_line(
            15,
            70_000,
            829,
            131_072,
            Some(agents::investigation::AdmissionReserveMeasurements {
                choice: wire(110_000),
                finish_only: wire(104_000),
            }),
        );
        assert_eq!(
            admitted,
            "capacity_trace kind=read_evidence call=15 current_payload=70000 source_bytes=829 ceiling=131072 choice=110000 finish_only=104000 reserve=40000 headroom=21072"
        );
        assert!(
            capacity_trace_line(15, 73_746, 1_822, 131_072, None)
                .ends_with("current_payload=73746 source_bytes=1822 ceiling=131072 refused")
        );
    }
}
