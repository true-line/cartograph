//! Real captured-source Invocation fixture. Initial task construction is test
//! setup; response admission and source access use production boundaries.
use super::*;
use llm::bounded::{CompletionLimits, PreparedBoundedCompletion};
use serde_json::{Value, json};

pub(crate) struct CapturedSourceInvocationFixture {
    inner: completion::InternalFixture,
    target: InvestigationInspectionTarget,
    current: InvestigationInput,
    prepared: PreparedBoundedCompletion,
    proof: UnreadSourcePreviewBound,
    response: InvestigationResponseMeta,
    action: Value,
}
impl CapturedSourceInvocationFixture {
    pub(crate) fn new(
        initialize: impl FnOnce(&std::path::Path) -> SourceValidationInitialData,
    ) -> Self {
        Self::with_gated(false, initialize)
    }
    pub(crate) fn with_gated(
        gated: bool,
        initialize: impl FnOnce(&std::path::Path) -> SourceValidationInitialData,
    ) -> Self {
        let (f, target, raw) = if gated {
            source_validation::captured_source_invocation_for(true, initialize)
        } else {
            source_validation::captured_source_invocation(initialize)
        };
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
    /// Negative binding tests only: actual authenticated reservation writer,
    /// scripted retention callback; grants no host source access.
    pub(crate) fn reserve_for_binding_test(&mut self) -> CommittedSourceReservation {
        let response = self.response.clone();
        self.inner
            .store
            .admit_next_prompt_source(
                &self.inner._execution_guard,
                self.inner.task.detail.summary.revision,
                &SourceAdmission {
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
    pub(crate) fn lose_charge_ack(&self) {
        source_validation::lose_next_charge_ack();
    }
    pub(crate) fn cancel_after_charge(&self) -> impl FnOnce() + 'static {
        let path = self.inner.directory.path().join("state.db");
        let id = self.inner.basis.task_id.clone();
        move || {
            JobStore::open(path)
                .unwrap()
                .cancel_next_prompt_investigation(&id)
                .unwrap();
        }
    }
    pub(crate) fn id(&self) -> &str {
        &self.inner.basis.task_id
    }
    pub(crate) fn request(&self) -> &StagedObligationRequest {
        &self.inner.request
    }
    pub(crate) fn current(&self) -> &InvestigationInput {
        &self.current
    }
    pub(crate) fn prepared(&self) -> &PreparedBoundedCompletion {
        &self.prepared
    }
    pub(crate) fn proof(&self) -> &UnreadSourcePreviewBound {
        &self.proof
    }
    pub(crate) fn admission(&self) -> SourceAdmission<'_> {
        SourceAdmission {
            response: &self.response,
            original_action: &self.action,
            current: &self.current,
            current_prepared: &self.prepared,
            proof: &self.proof,
            target: &self.target,
            active_milliseconds: self.response.active_milliseconds,
        }
    }
    pub(crate) fn execution(&self) -> &JobExecution {
        &self.inner._execution_guard
    }
    pub(crate) fn revision(&self) -> u64 {
        self.inner.task.detail.summary.revision
    }
    pub(crate) fn cancel(&mut self) -> u64 {
        self.inner
            .store
            .cancel_next_prompt_investigation(&self.inner.basis.task_id)
            .unwrap()
            .summary
            .revision
    }
    pub(crate) fn disposition_after_reopen(&self) -> String {
        let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let read =
            read_reservations::load(&tx, &self.inner.basis.task_id, &self.inner.step.step_id)
                .unwrap()
                .unwrap();
        serde_json::to_string(&read.disposition.unwrap().kind).unwrap()
    }
    pub(crate) fn publication_after_reopen(&self) -> (u64, usize, usize) {
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
    pub(crate) fn counts(&self) -> (usize, usize, u64) {
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
