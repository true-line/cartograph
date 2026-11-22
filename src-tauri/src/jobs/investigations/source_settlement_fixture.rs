//! Explicit internal reservation seed; real builders, seals and owner handles.
use super::*;
use llm::bounded::{BoundedCallError, CompletionLimits, ProviderProfile};
use llm::{EgressFirewall, EgressPolicy, LlmProvider, Locality, ProviderCaps};

struct Offline;
impl LlmProvider for Offline {
    fn id(&self) -> &str {
        "ollama:test-model"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        Ok(ProviderProfile {
            protocol_version: llm::bounded::PROTOCOL_VERSION.into(),
            provider_id: self.id().into(),
            locality: Locality::Local,
            endpoint_id: "http://127.0.0.1:11434/api/chat".into(),
            requested_model: "test-model".into(),
        })
    }
    fn embed(&self, _: &[String]) -> Result<Vec<llm::Embedding>, llm::ProviderError> {
        panic!("no provider dispatch")
    }
}
pub(super) fn prepared(input: &InvestigationInput, id: String) -> PreparedBoundedCompletion {
    EgressFirewall::new(EgressPolicy::default())
        .prepare_bounded(
            &Offline,
            &input.action(id).unwrap(),
            &CompletionLimits::default(),
        )
        .unwrap()
}
pub(super) struct Fixture {
    pub f: completion::InternalFixture,
    pub r: read_reservations::Reservation,
    pub current: InvestigationInput,
    pub prepared: PreparedBoundedCompletion,
    pub proof: UnreadSourcePreviewBound,
    raw: String,
    basis: InvestigationTurnBasis,
}
impl Fixture {
    pub(super) fn new() -> Self {
        Self::with_gated(false)
    }
    pub(super) fn gated() -> Self {
        Self::with_gated(true)
    }
    fn with_gated(gated: bool) -> Self {
        let (mut f, mut r) = if gated {
            source_validation::gated_source_obligation_fixture()
        } else {
            source_validation::internal_source_obligation_fixture()
        };
        let policy = &INVESTIGATION_V2_POLICY;
        let raw=serde_json::json!({"context":{"context_id":f.request.expected_context_id,
            "coverage":f.ledger.coverage.as_ref().unwrap().qualification,
            "response":{"snapshot_id":f.ledger.scope_snapshot_id,"view":"recovered_graph","total_selected":1,
            "facts":[{"reference":r.source.fact,"properties":{"note":"bounded native fixture"}}],"next_cursor":null}},
            "evidence_options":[{"fact":r.source.fact,"origin":"captured_primary_source_metadata","ranges":[]}]}).to_string();
        assert_eq!(
            core_prov::content_hash(raw.as_bytes()),
            f.ledger.queries[0].response_hash
        );
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let basis = turn::basis(&tx, &f.task).unwrap();
        let current = InvestigationInput::prepare_v9_with_policy(
            &f.request.question,
            f.task.detail.summary.specialist_id,
            f.ledger.clone(),
            &[raw.as_str()],
            &[],
            &[],
            &f.basis.admission.consumed_budgets,
            &basis,
            &f.request,
            &f.metadata,
            &[],
            CompletionCallMode::Choice,
            0,
            None,
            policy,
        )
        .unwrap();
        let current = if gated {
            let choices = completion::SavedMetadataHistory::load(&tx, &f.task)
                .unwrap()
                .acquisition_choices_at(&f.basis, &f.ledger)
                .unwrap()
                .unwrap();
            current
                .with_acquisition_admission(std::sync::Arc::new(choices), None)
                .unwrap()
        } else {
            current
        };
        let prepared = prepared(&current, format!("{}:{}", f.basis.task_id, f.step.step_id));
        assert_eq!(
            current
                .seal_disclosure_with_policy(&prepared, policy)
                .unwrap(),
            f.step.disclosure
        );
        let next_id = format!(
            "{}:step-{}",
            f.basis.task_id,
            r.binding.base_usage.model_invocations + 1
        );
        let proof = current
            .certify_unread_source_v9_with_policy(
                &prepared,
                &r.source,
                prepared.profile(),
                prepared.limits(),
                &next_id,
                policy,
            )
            .unwrap();
        r.binding.certification_basis_hash = proof.basis_hash().into();
        drop(tx);
        replace_reservation(&mut f, &r);
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let response = read_body::<InvestigationResponseMeta>(
            &tx,
            "investigation_responses",
            "step_id",
            &f.basis.task_id,
            &f.step.step_id,
        )
        .unwrap()
        .unwrap();
        let bound = envelope(&tx, &f.task, &r, &response).unwrap();
        r.storage_claim.validation_bytes =
            source_validation::validation_storage_bound(&tx, &f.task, &r).unwrap();
        r.storage_claim.settlement_bytes = bound.bytes;
        r.storage_claim.settlement_attempts = bound.attempts;
        drop(tx);
        replace_reservation(&mut f, &r);
        Self {
            f,
            r,
            current,
            prepared,
            proof,
            raw,
            basis,
        }
    }
    pub(super) fn charge(&mut self) {
        let hash = self.r.fingerprint().unwrap();
        self.f
            .store
            .charge_prepared_source_validation(
                &self.f._execution_guard,
                &source_validation::SourceValidationBasis {
                    task_id: &self.f.basis.task_id,
                    step_id: &self.f.step.step_id,
                    reservation_hash: &hash,
                    revision: self.f.task.detail.summary.revision,
                },
                &self.r.source,
                &self.r.binding.base_ledger_hash,
            )
            .unwrap();
        self.refresh();
    }
    pub(super) fn refresh(&mut self) {
        let tx = self.f.store.conn.unchecked_transaction().unwrap();
        self.f.task = load_next_prompt_task_base(&tx, &self.f.basis.task_id).unwrap();
    }
    pub(super) fn raw(&self) -> &str {
        &self.raw
    }
    pub(super) fn response(&self) -> InvestigationResponseMeta {
        let tx = self.f.store.conn.unchecked_transaction().unwrap();
        read_body(
            &tx,
            "investigation_responses",
            "step_id",
            &self.f.basis.task_id,
            &self.f.step.step_id,
        )
        .unwrap()
        .unwrap()
    }
    pub(super) fn candidate(
        &self,
        reason: Option<UnavailableReason>,
    ) -> (InvestigationInput, PreparedBoundedCompletion) {
        self.candidate_with(reason, None)
    }
    /// With `refusal`, the acquired read is withheld exactly as the worker
    /// withholds a firewall-refused read, and its refusal joins the metadata.
    pub(super) fn candidate_with(
        &self,
        reason: Option<UnavailableReason>,
        refusal: Option<completion::read_refusals::ReadDecider<'_>>,
    ) -> (InvestigationInput, PreparedBoundedCompletion) {
        let response = self.response();
        let tx = self.f.store.conn.unchecked_transaction().unwrap();
        let saved = source_validation::validate_saved_pending(&tx, &self.f.task).unwrap();
        let mut ledger = self.f.ledger.clone();
        let mut evidence = vec![];
        let kind = if reason.is_some() {
            DispositionKind::ReadUnavailable
        } else {
            DispositionKind::Acquired
        };
        let turn = if reason.is_none() {
            let source = &self.r.source;
            ledger.revision += 1;
            ledger.citations.push(InvestigationCitation {
                citation_id: "evidence-1".into(),
                fact: source.fact.clone(),
                fact_digest: source.fact_digest.clone(),
                source: Some(source.source.clone()),
                role: Some(source.role),
                index: Some(source.index),
                text_hash: Some(core_prov::content_hash(b"private text")),
                origin: InvestigationEvidenceOrigin::CapturedPrimarySource {
                    registered_source_id: source.registered_source_id.clone(),
                    receipt_id: source.receipt_id.clone(),
                    receipt_inventory_index: source.receipt_inventory_index,
                    captured: source.captured.clone(),
                    scope: agents::PrimarySourceScope::PrimarySourceOnly,
                    input_closure: agents::InputClosureStatus::InputClosureNotEstablished,
                },
            });
            evidence.push(agents::AgentEvidence {
                id: "evidence-1".into(),
                source: source.source.clone(),
                text: "private text".into(),
            });
            InvestigationTurnBasis {
                input_ledger_hash: ledger
                    .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                    .unwrap(),
                previous_ledger: Some(self.f.ledger.clone()),
                completed_tool: Some(InvestigationTool::ReadEvidence),
            }
        } else {
            self.basis.clone()
        };
        let outcome = reason
            .map(|reason| AttemptOutcome::Unavailable { reason })
            .unwrap_or(AttemptOutcome::SourceAcquired {
                citation_id: "evidence-1".into(),
            });
        let records = completion::derive_attempt_records(
            &self.f.task,
            &self.f.step.step_id,
            &response,
            &self.r.binding.original_read,
            &outcome,
            Some(&ledger),
            self.f.metadata.completed_attempt_ids.len(),
        )
        .unwrap();
        let refused = refusal.and_then(|decide| {
            completion::read_refusals::plan_settlement_refusals(
                &self.f.task,
                &records,
                &ledger,
                decide,
            )
            .unwrap()
        });
        let mut metadata = self.f.metadata.clone();
        metadata
            .completed_attempt_ids
            .extend(records.iter().map(|r| r.attempt_id.clone()));
        let mut withheld = std::collections::BTreeSet::new();
        if let Some(refused) = &refused {
            metadata
                .completed_attempt_ids
                .extend(refused.iter().map(|r| r.record.attempt_id.clone()));
            withheld.insert("evidence-1".to_string());
            evidence.clear();
        }
        let usage = usage_for(
            &saved,
            kind,
            self.f.task.detail.usage.active_milliseconds + 1,
        )
        .unwrap();
        let input = InvestigationInput::prepare_v9_withholding_with_policy(
            &self.f.request.question,
            self.f.task.detail.summary.specialist_id,
            ledger,
            &[self.raw.as_str()],
            &evidence,
            &[],
            &usage,
            &turn,
            &self.f.request,
            &metadata,
            &[],
            CompletionCallMode::Choice,
            0,
            None,
            &INVESTIGATION_V2_POLICY,
            &withheld,
        )
        .unwrap();
        let input = if self.f.task.detail.summary.specialist_id == SpecialistId::DomainAnalystV18 {
            drop(tx);
            self.f
                .store
                .next_source_continuation_snapshot(&SourceContinuationSnapshotInput {
                    investigation_id: &self.f.basis.task_id,
                    expected_revision: self.f.task.detail.summary.revision,
                    request: &self.f.request,
                    candidate_ledger: input.ledger(),
                    outcome: if kind == DispositionKind::Acquired {
                        SourceContinuationOutcome::Acquired
                    } else {
                        SourceContinuationOutcome::Unavailable(match outcome {
                            AttemptOutcome::Unavailable { reason } => reason,
                            _ => unreachable!(),
                        })
                    },
                    active_milliseconds: usage.active_milliseconds,
                    refusal,
                })
                .unwrap()
                .attach_acquisition(input)
                .unwrap()
        } else {
            input
        };
        let prepared = prepared(
            &input,
            format!(
                "{}:step-{}",
                self.f.basis.task_id,
                self.r.binding.base_usage.model_invocations + 1
            ),
        );
        (input, prepared)
    }
}
/// Test-only explicit reservation construction, before production admission
/// exists. No saved invocation/disclosure is invented or replaced.
pub(super) fn replace_reservation(
    f: &mut completion::InternalFixture,
    r: &read_reservations::Reservation,
) {
    let tx = f.store.conn.transaction().unwrap();
    tx.execute_batch("DROP TRIGGER investigation_read_reservations_no_update")
        .unwrap();
    let bytes = encode(r, MAX_MANIFEST_BYTES).unwrap();
    tx.execute(
        "UPDATE investigation_read_reservations SET body=?2,body_hash=?3 WHERE task_id=?1",
        params![f.basis.task_id, bytes, core_prov::content_hash(&bytes)],
    )
    .unwrap();
    tx.execute_batch("CREATE TRIGGER investigation_read_reservations_no_update BEFORE UPDATE ON investigation_read_reservations BEGIN SELECT RAISE(ABORT, 'immutable investigation read record'); END").unwrap();
    f.task
        .pending_source_read
        .as_mut()
        .unwrap()
        .reservation_hash = r.fingerprint().unwrap();
    let bytes = encode(&f.task, MAX_RECORD_BYTES).unwrap();
    tx.execute(
        "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
        params![f.basis.task_id, bytes, core_prov::content_hash(&bytes)],
    )
    .unwrap();
    source_validation::validate_saved_pending(&tx, &f.task).unwrap();
    tx.commit().unwrap();
}
