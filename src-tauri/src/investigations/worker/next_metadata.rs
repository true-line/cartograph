//! Internal Choice metadata reads use exact previews and one atomic publication.
use super::super::context::ContextReadPreparationError;
use super::*;
use crate::investigations::context::metadata_admission::MetadataPublicationError;
use crate::jobs::investigations::{MetadataAdmission, MetadataContinuationRead};

impl<R: tauri::Runtime> Worker<'_, R> {
    /// False identifies a source proposal for the separate pre-read reservation
    /// path. It must never fall through to a legacy tool transition.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn admit_next_metadata_response(
        &mut self,
        request: &obligations::StagedObligationRequest,
        current: &InvestigationInput,
        current_prepared: &PreparedBoundedCompletion,
        completion: &BoundedCompletion,
        step_id: &str,
        provider: &dyn LlmProvider,
        context: &mut FrozenContext,
        history: &[String],
    ) -> Result<bool, Failure> {
        let effective_policy = crate::jobs::investigations::policy_for_detail(&self.detail)?;
        let policy = &effective_policy;
        let proof = current
            .ledger()
            .coverage
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        let action = ProgressiveInvestigationAction::decode_resolved_detailed_for_specialist(
            &completion.text,
            &proof.context_id,
            &request.intent_hash_with_policy(policy)?,
            current.ledger(),
            policy,
            self.detail.summary.specialist_id,
        )
        .map_err(|failure| Failure::Stop(InvestigationStopReason::ActionDecode(failure)))?;
        self.check_new_work()?;
        let original: serde_json::Value = serde_json::from_str(&completion.text)
            .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
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
        let (read, kind) = match action {
            CompletionInvestigationAction::Read(QualifiedInvestigationAction::QueryContext {
                query,
                ..
            }) => {
                // #1352: a query whose exact page is already in the context adds
                // nothing. Acquisition-choice specialists are refused earlier as
                // repeated_read and continue; a specialist without that closed
                // feedback is stopped before it spends the query bytes again.
                if !self.detail.summary.specialist_id.uses_acquisition_choices()
                    && current
                        .ledger()
                        .queries
                        .iter()
                        .any(|answered| answered.query == query)
                {
                    return Err(Failure::Stop(InvestigationStopReason::HostLimitAt {
                        phase: PreparationPhase::Query,
                        limit: HostLimitKind::RepeatedQuery,
                    }));
                }
                let prepared = context.prepare_query_with_diagnostic(self.state, query.clone());
                let read = match prepared {
                    Ok(read) => read,
                    Err(ContextReadPreparationError::QueryBudget(budget))
                        if current.is_maximum_obligation_query(&query) =>
                    {
                        // The offered exact node already uses MAX_QUERY_BYTES.
                        // No model action can make this obligation page fit.
                        return Err(Failure::Stop(InvestigationStopReason::HostLimitAt {
                            phase: PreparationPhase::Query,
                            limit: HostLimitKind::QueryPageBudget(budget),
                        }));
                    }
                    Err(
                        error @ (ContextReadPreparationError::QueryRejected(_)
                        | ContextReadPreparationError::QueryBudget(_)),
                    ) if self.detail.summary.specialist_id.uses_acquisition_choices() => {
                        use agents::investigation::query_refusal::QueryRefusalRecord;
                        let (reason, budget) = match error {
                            ContextReadPreparationError::QueryRejected(reason) => (reason, None),
                            ContextReadPreparationError::QueryBudget(budget) => (
                                agents::investigation::query_refusal::QueryRejection::PageBudget,
                                Some(budget),
                            ),
                            _ => unreachable!(),
                        };
                        let mut record = QueryRefusalRecord {
                            budget,
                            schema_version: if budget.is_some() { 3 } else { 2 },
                            record_id: String::new(),
                            task_id: self.id.into(),
                            step_id: step_id.into(),
                            action_hash: response.action_hash.clone(),
                            input_ledger_hash: current.ledger().fingerprint_with_policy(policy)?,
                            input_revision: current.ledger().revision,
                            context_id: Some(proof.context_id.clone()),
                            query,
                            reason,
                        };
                        record.record_id = record.content_id()?;
                        self.check_new_work()?;
                        self.detail = self
                            .state
                            .jobs
                            .lock()
                            .map_err(|_| HostError::Operational)?
                            .refuse_next_query(
                                self.execution,
                                self.detail.summary.revision,
                                crate::jobs::investigations::QueryRefusalAdmission {
                                    response: &response,
                                    original_action: &original,
                                    current,
                                    current_prepared,
                                    record,
                                    active_milliseconds: self.clock.active_milliseconds(),
                                },
                            )?;
                        super::super::emit_changed(self.app, &self.detail);
                        return Ok(true);
                    }
                    Err(ContextReadPreparationError::QueryBudget(budget)) => {
                        return Err(Failure::Stop(InvestigationStopReason::HostLimitAt {
                            phase: PreparationPhase::Query,
                            limit: HostLimitKind::QueryPageBudget(budget),
                        }));
                    }
                    Err(ContextReadPreparationError::CumulativeQueryAndInventoryBytes) => {
                        return Err(Failure::Stop(InvestigationStopReason::HostLimitAt {
                            phase: PreparationPhase::Query,
                            limit: HostLimitKind::CumulativeQueryAndInventoryBytes,
                        }));
                    }
                    Err(error) => {
                        return Err(Failure::from(error.into_host_error())
                            .preparation_at(PreparationPhase::Query));
                    }
                };
                (read, ContextPreviewKind::Query)
            }
            CompletionInvestigationAction::Read(
                QualifiedInvestigationAction::ReadCoverageInventory(read),
            ) => (
                context
                    .prepare_inventory_with_diagnostic(read)
                    .map_err(|error| match error {
                        ContextReadPreparationError::CumulativeQueryAndInventoryBytes => {
                            Failure::Stop(InvestigationStopReason::HostLimitAt {
                                phase: PreparationPhase::Inventory,
                                limit: HostLimitKind::CumulativeQueryAndInventoryBytes,
                            })
                        }
                        ContextReadPreparationError::QueryRejected(_)
                        | ContextReadPreparationError::QueryBudget(_) => {
                            Failure::from(HostError::InvalidInput)
                                .preparation_at(PreparationPhase::Inventory)
                        }
                        ContextReadPreparationError::Host(error) => {
                            Failure::from(error).preparation_at(PreparationPhase::Inventory)
                        }
                    })?,
                ContextPreviewKind::Inventory,
            ),
            CompletionInvestigationAction::Read(QualifiedInvestigationAction::SourceOrFinish(
                _,
            )) => return Ok(false),
            CompletionInvestigationAction::Finish(_) => {
                return Err(Failure::Stop(InvestigationStopReason::InvalidResponse));
            }
        };
        let active_milliseconds = self.clock.active_milliseconds();
        let snapshot = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .investigation_metadata_continuation_snapshot(
                self.id,
                self.detail.summary.revision,
                &MetadataContinuationRead {
                    request,
                    response: &response,
                    original_action: &original,
                    current,
                    current_prepared,
                    candidate_ledger: read.ledger(),
                    raw_response: read.raw(),
                    kind,
                    active_milliseconds,
                },
            )
            .map_err(|error| Failure::from(error).preparation_at(PreparationPhase::Candidate))?;
        let view = context
            .prospective_context(&read)
            .map_err(|error| Failure::from(error).preparation_at(PreparationPhase::Candidate))?;
        let candidate = InvestigationInput::prepare_v9_with_policy(
            &request.question,
            self.detail.summary.specialist_id,
            view.ledger.clone(),
            &view.query_pages,
            view.evidence,
            history,
            snapshot.usage(),
            snapshot.basis(),
            request,
            snapshot.metadata(),
            &view.inventory_details,
            snapshot.mode(),
            snapshot.capacity_refusal_count(),
            None, // An accepted acquisition clears only the latest refusal slot.
            policy,
        );
        let candidate = candidate.and_then(|candidate| snapshot.attach_acquisition(candidate));
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(InvestigationError::LimitExceeded) => {
                self.refuse_next_metadata_capacity(&response, &original)?;
                return Ok(true);
            }
            Err(error) => {
                return Err(Failure::from(error).preparation_at(PreparationPhase::Candidate));
            }
        };
        // The certificate reserves both continuation modes and feedback through
        // the same production serializer/redactor. It is not a storage permit.
        let proof = current.certify_context_preview_v9_with_policy(
            &candidate,
            kind,
            read.raw(),
            snapshot.admitted_usage(),
            current_prepared.profile(),
            current_prepared.limits(),
            snapshot.next_action_id(),
            policy,
        );
        let proof = match proof {
            Ok(proof) => proof,
            Err(InvestigationError::LimitExceeded) => {
                self.refuse_next_metadata_capacity(&response, &original)?;
                return Ok(true);
            }
            Err(error) => {
                return Err(Failure::from(error).preparation_at(PreparationPhase::Certification));
            }
        };
        let action = candidate
            .action(snapshot.next_action_id().into())
            .map_err(|error| Failure::from(error).preparation_at(PreparationPhase::Candidate))?;
        let candidate_prepared = firewall(self.state)?
            .prepare_bounded(provider, &action, current_prepared.limits())
            .map_err(provider_failure)?;
        // Size certification and actual protected-field sealing are independent.
        candidate
            .seal_disclosure_with_policy(&candidate_prepared, policy)
            .map_err(|error| Failure::from(error).preparation_at(PreparationPhase::Seal))?;
        let result = context.publish_admitted_metadata(
            self.state,
            self.execution,
            snapshot.revision(),
            self.clock,
            read,
            &MetadataAdmission {
                response: &response,
                original_action: &original,
                current,
                candidate: &candidate,
                current_prepared,
                candidate_prepared: &candidate_prepared,
                proof: &proof,
                kind,
                active_milliseconds,
            },
        );
        self.detail = match result {
            Ok(detail) => detail,
            Err(MetadataPublicationError::Host(error)) => {
                return Err(Failure::from(error).preparation_at(PreparationPhase::Publication));
            }
            Err(MetadataPublicationError::Store(error)) => return Err(error.into()),
        };
        super::super::emit_changed(self.app, &self.detail);
        Ok(true)
    }

    fn refuse_next_metadata_capacity(
        &mut self,
        response: &InvestigationResponseMeta,
        original: &serde_json::Value,
    ) -> Result<(), Failure> {
        // Preparation and storage/tool limits never enter this branch. Keep the
        // exact original model response; no query/replay rewrite is persisted.
        self.check_new_work()?;
        self.detail = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .refuse_next_prompt_capacity(
                self.execution,
                self.detail.summary.revision,
                response,
                original,
                CapacityRefusalReason::PromptCapacity,
            )?;
        super::super::emit_changed(self.app, &self.detail);
        Ok(())
    }
}
