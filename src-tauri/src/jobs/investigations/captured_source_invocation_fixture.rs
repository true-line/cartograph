//! Real captured-source Invocation fixture. Initial task construction is test
//! setup; response admission and source access use production boundaries.
use super::*;
use llm::bounded::{CompletionLimits, PreparedBoundedCompletion};
use serde_json::{Value, json};

pub struct CapturedSourceInvocationFixture {
    inner: completion::InternalFixture,
    target: InvestigationInspectionTarget,
    current: InvestigationInput,
    prepared: PreparedBoundedCompletion,
    proof: UnreadSourcePreviewBound,
    response: InvestigationResponseMeta,
    action: Value,
}
impl CapturedSourceInvocationFixture {
    pub fn new(initialize: impl FnOnce(&std::path::Path) -> SourceValidationInitialData) -> Self {
        Self::with_gated(false, initialize)
    }
    pub fn with_gated(
        gated: bool,
        initialize: impl FnOnce(&std::path::Path) -> SourceValidationInitialData,
    ) -> Self {
        Self::with_role(gated.then_some(SpecialistId::DomainAnalystV18), initialize)
    }
    pub fn with_role(
        role: Option<SpecialistId>,
        initialize: impl FnOnce(&std::path::Path) -> SourceValidationInitialData,
    ) -> Self {
        let gated = role.is_some();
        let (f, target, raw) =
            source_validation::captured_source_invocation_for_role(role, initialize);
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let basis = turn::basis(&tx, &f.task).unwrap();
        tx.commit().unwrap();
        let policy = &INVESTIGATION_V2_POLICY;
        let current = InvestigationInput::prepare_v9_with_policy(
            &f.request.question,
            f.task.detail.summary.specialist_id,
            f.ledger.clone(),
            &[&raw],
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
            let tx = f.store.conn.unchecked_transaction().unwrap();
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
        // Construction exposes a local profile only; no invocation/transport.
        let provider = llm::OllamaProvider::new(
            "http://127.0.0.1:11434",
            "test-model",
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        let prepared = llm::EgressFirewall::new(llm::EgressPolicy::default())
            .prepare_bounded(
                &provider,
                &current
                    .action(format!("{}:{}", f.basis.task_id, f.step.step_id))
                    .unwrap(),
                &CompletionLimits::default(),
            )
            .unwrap();
        assert_eq!(
            current
                .seal_disclosure_with_policy(&prepared, policy)
                .unwrap(),
            f.step.disclosure
        );
        let proof = current
            .certify_unread_source_v9_with_policy(
                &prepared,
                &target,
                prepared.profile(),
                prepared.limits(),
                &format!(
                    "{}:step-{}",
                    f.basis.task_id,
                    f.task.detail.usage.model_invocations + 1
                ),
                policy,
            )
            .unwrap();
        let action = json!({"type":"read_evidence","fact":target.fact,"role":target.role,"index":target.index});
        let response = InvestigationResponseMeta {
            finish_feedback_witness: None,
            finish_telemetry: None,
            step_id: f.step.step_id.clone(),
            response_hash: core_prov::content_hash(b"scripted captured-source complete action"),
            action_hash: core_prov::content_hash(&encode(&action, MAX_ACTION_BYTES).unwrap()),
            observed_model: None,
            reported_input_tokens: Some(17),
            reported_output_tokens: Some(9),
            reported_cache_read_tokens: None,
            active_milliseconds: f.task.detail.usage.active_milliseconds + 2,
        };
        Self {
            inner: f,
            target,
            current,
            prepared,
            proof,
            response,
            action,
        }
    }
    /// Submit a real finish after the captured source has been published. No transport.
    pub fn assert_configuration_refusal_after_capture(
        &mut self,
        current: &InvestigationInput,
        prepared: &PreparedBoundedCompletion,
        provider: &dyn llm::LlmProvider,
        firewall: &llm::EgressFirewall,
        citation: &str,
    ) {
        let before = self.inner.store.investigation(self.id()).unwrap();
        // Source-publication fixture limits are not a real invocation profile.
        // Reprepare the same acquired input using the saved task's exact policy.
        let tx = self.inner.store.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task(&tx, self.id()).unwrap();
        let policy = super::policy_for_detail(&task.detail).unwrap();
        let mut limits = CompletionLimits::default().values();
        limits.request_timeout_ms = task
            .completion_intent
            .as_ref()
            .unwrap()
            .effective_request_timeout_ms();
        limits.max_output_tokens = policy.limits().generated_tokens_per_invocation;
        limits.max_spans = u32::try_from(policy.limits().evidence_items).unwrap();
        drop(tx);
        let action = current
            .action(format!(
                "{}:step-{}",
                self.id(),
                before.usage.model_invocations + 1
            ))
            .unwrap();
        let invocation_prepared = firewall
            .prepare_bounded(provider, &action, &CompletionLimits::new(limits).unwrap())
            .unwrap();
        // Preserve the captured-source profile while applying invocation limits.
        assert_eq!(invocation_prepared.profile(), prepared.profile());
        let prepared = &invocation_prepared;

        let sealed_active = current
            .seal_disclosure_with_policy(prepared, &INVESTIGATION_V2_POLICY)
            .unwrap()
            .unwrap()
            .admission
            .unwrap()
            .consumed_budgets
            .active_milliseconds;

        let authorized = firewall
            .authorize_bounded(provider, prepared, None)
            .unwrap();
        let invoked = self
            .inner
            .store
            .begin_next_prompt_invocation(
                &self.inner._execution_guard,
                before.summary.revision,
                &NextPromptInvocation {
                    input: current,
                    prepared,
                    active_milliseconds: sealed_active,
                },
                &authorized,
                |_| Ok(()),
            )
            .unwrap();
        let original = json!({"schema_version":1,"outcome":"supported",
            "action":{"type":"finish","findings":[{
                "claim_kind":"inferred_interpretation","title":"Default MissingStrategy",
                "statement":"MissingStrategy controls this behavior.","citation_ids":[citation],
                "limitations":["Only selected material was considered."]
            }],"knowledge_completeness":"partial","limitations":["Only selected material was considered."]}});
        let response = InvestigationResponseMeta {
            finish_feedback_witness: None,
            finish_telemetry: None,
            step_id: format!("step-{}", invoked.usage.model_invocations),
            response_hash: core_prov::content_hash(b"scripted unsupported configuration claim"),
            action_hash: core_prov::content_hash(&encode(&original, MAX_ACTION_BYTES).unwrap()),
            observed_model: None,
            reported_input_tokens: Some(17),
            reported_output_tokens: Some(9),
            reported_cache_read_tokens: None,
            active_milliseconds: invoked.usage.active_milliseconds + 1,
        };
        let detail = self
            .inner
            .store
            .admit_next_prompt_finish(
                &self.inner._execution_guard,
                invoked.summary.revision,
                &NextPromptFinish {
                    request: &self.inner.request,
                    response: &response,
                    original_action: &original,
                    current,
                    current_prepared: prepared,
                },
            )
            .unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Running);
        assert!(!detail.summary.has_result);
        let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation(self.id()).unwrap(), detail);
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task(&tx, self.id()).unwrap();
        let entry = staged::load_for_task(&tx, &task).unwrap().remove(0);
        assert_eq!(entry.record.feedback.schema_version, 5);
        let Some(
            agents::investigation::staged_finish::ProseCorrection::UnverifiedConfigurationClaimAt(
                location,
            ),
        ) = &entry.record.feedback.prose_correction
        else {
            panic!(
                "expected configuration refusal: {:?}",
                entry.record.feedback
            );
        };
        assert_eq!(
            location.start.field,
            agents::investigation::ProseField::FindingTitle { finding: 0 }
        );
        assert_eq!(location.start.sentence, 1);
        assert!(entry.response.finish_feedback_witness.is_some());
        assert!(
            !String::from_utf8(entry.record.feedback.bytes().unwrap())
                .unwrap()
                .contains("MissingStrategy")
        );
        assert!(staged::load_for_task(&tx, &task).is_ok());
        drop(tx);
        for mutation in [
            "missing_digest",
            "changed_digest",
            "changed_feedback",
            "missing_response",
        ] {
            let tx = reopened.conn.unchecked_transaction().unwrap();
            let mut entry = staged::read::<staged::JournalEntry>(
                &tx,
                "staged_completion_refusals",
                self.id(),
                Some(1),
                MAX_RECORD_BYTES,
            )
            .unwrap()
            .unwrap();
            assert!(entry.response.finish_feedback_witness.is_some());
            for table in ["investigation_responses", "staged_completion_refusals"] {
                let trigger = format!(
                    "{table}_no_{}",
                    if mutation == "missing_response" && table == "investigation_responses" {
                        "delete"
                    } else {
                        "update"
                    }
                );
                let sql: String = tx
                    .query_row(
                        "SELECT sql FROM sqlite_schema WHERE name=?1",
                        [&trigger],
                        |r| r.get(0),
                    )
                    .unwrap();
                tx.execute_batch(&format!("DROP TRIGGER {trigger}"))
                    .unwrap();
                if table == "investigation_responses" {
                    match mutation {
                        "missing_digest" => entry.response.finish_feedback_witness = None,
                        "changed_digest" => {
                            entry.response.finish_feedback_witness = Some("f".repeat(64))
                        }
                        _ => (),
                    }
                    if mutation == "missing_response" {
                        tx.execute(
                            "DELETE FROM investigation_responses WHERE task_id=?1 AND step_id=?2",
                            params![entry.record.task_id, entry.record.step_id],
                        )
                        .unwrap();
                    } else {
                        let body = encode(&entry.response, 4096).unwrap();
                        tx.execute("UPDATE investigation_responses SET body=?3,body_hash=?4 WHERE task_id=?1 AND step_id=?2", params![entry.record.task_id,entry.record.step_id,body,core_prov::content_hash(&body)]).unwrap();
                    }
                } else {
                    if mutation == "changed_feedback" {
                        if let Some(agents::investigation::staged_finish::ProseCorrection::UnverifiedConfigurationClaimAt(location)) = &mut entry.record.feedback.prose_correction {
                        location.start.sentence += 1;
                        location.end.sentence += 1;
                    } else { panic!("expected schema5 configuration correction"); }
                        entry.record.refusal_id = entry.record.content_id().unwrap();
                    }
                    entry.rehash_for_test();
                    let body = encode(&entry, MAX_RECORD_BYTES).unwrap();
                    tx.execute("UPDATE staged_completion_refusals SET body=?2,body_hash=?3 WHERE task_id=?1 AND sequence=1", params![entry.record.task_id,body,core_prov::content_hash(&body)]).unwrap();
                }
                tx.execute_batch(&sql).unwrap();
            }
            assert!(staged::load_for_task(&tx, &task).is_err(), "{mutation}");
            // Drop rolls back mutation and restored triggers before the next case.
        }
    }
    /// Negative binding tests only: actual authenticated reservation writer,
    /// scripted retention callback; grants no host source access.
    pub fn reserve_for_binding_test(&mut self) -> CommittedSourceReservation {
        let response = self.response.clone();
        self.inner
            .store
            .admit_next_prompt_source(
                &self.inner._execution_guard,
                self.inner.task.detail.summary.revision,
                &SourceAdmission {
                    size_binding: None,
                    response: &response,
                    original_action: &self.action,
                    current: &self.current,
                    current_prepared: &self.prepared,
                    proof: &self.proof,
                    target: &self.target,
                    active_milliseconds: response.active_milliseconds,
                },
                |_| Ok(()),
            )
            .unwrap()
    }
    pub fn lose_charge_ack(&self) {
        source_validation::lose_next_charge_ack();
    }
    pub fn cancel_after_charge(&self) -> impl FnOnce() + 'static {
        let path = self.inner.directory.path().join("state.db");
        let id = self.inner.basis.task_id.clone();
        move || {
            JobStore::open(path)
                .unwrap()
                .cancel_next_prompt_investigation(&id)
                .unwrap();
        }
    }
    pub fn id(&self) -> &str {
        &self.inner.basis.task_id
    }
    pub fn request(&self) -> &StagedObligationRequest {
        &self.inner.request
    }
    pub fn current(&self) -> &InvestigationInput {
        &self.current
    }
    pub fn prepared(&self) -> &PreparedBoundedCompletion {
        &self.prepared
    }
    pub fn use_measured_proof(&mut self, sizes: core_redact::DirectSourceJsonContentSizes) {
        self.proof = self
            .current
            .certify_measured_unread_source_v9_with_policy(
                &self.prepared,
                &self.target,
                self.prepared.profile(),
                self.prepared.limits(),
                &format!(
                    "{}:step-{}",
                    self.inner.basis.task_id,
                    self.inner.task.detail.usage.model_invocations + 1
                ),
                &INVESTIGATION_V2_POLICY,
                sizes,
            )
            .unwrap();
    }
    pub fn proof(&self) -> &UnreadSourcePreviewBound {
        &self.proof
    }
    pub fn admission(&self) -> SourceAdmission<'_> {
        SourceAdmission {
            size_binding: None,
            response: &self.response,
            original_action: &self.action,
            current: &self.current,
            current_prepared: &self.prepared,
            proof: &self.proof,
            target: &self.target,
            active_milliseconds: self.response.active_milliseconds,
        }
    }
    pub fn execution(&self) -> &JobExecution {
        &self.inner._execution_guard
    }
    pub fn revision(&self) -> u64 {
        self.inner.task.detail.summary.revision
    }
    pub fn cancel(&mut self) -> u64 {
        self.inner
            .store
            .cancel_next_prompt_investigation(&self.inner.basis.task_id)
            .unwrap()
            .summary
            .revision
    }
    pub fn disposition_after_reopen(&self) -> String {
        let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let read =
            read_reservations::load(&tx, &self.inner.basis.task_id, &self.inner.step.step_id)
                .unwrap()
                .unwrap();
        serde_json::to_string(&read.disposition.unwrap().kind).unwrap()
    }
    pub fn publication_after_reopen(&self) -> (u64, usize, usize) {
        let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task(&tx, &self.inner.basis.task_id).unwrap();
        let ledger = load_ledger(&tx, &task).unwrap().unwrap();
        let attempts = completion::attempts(&tx, &task).unwrap();
        (
            ledger.revision,
            task.detail.usage.evidence_items,
            attempts.len(),
        )
    }
    /// Reopen/authenticate saved authority independently of the AppState handle.
    pub fn counts(&self) -> (usize, usize, u64) {
        let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task_base(&tx, &self.inner.basis.task_id).unwrap();
        let saved = if read_reservations::validate_optional_schema(&tx).unwrap() {
            read_reservations::load(&tx, &self.inner.basis.task_id, &self.inner.step.step_id)
                .unwrap()
        } else {
            None
        };
        (
            usize::from(saved.is_some()),
            usize::from(saved.as_ref().is_some_and(|read| read.charge.is_some())),
            task.detail.usage.captured_validation_bytes,
        )
    }
}

#[path = "captured_withholding_fixture.rs"]
mod withholding;

#[path = "captured_patch_fixture.rs"]
mod patch;
