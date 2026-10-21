//! AC-0287: real saved source Invocation, no pre-inserted response/reservation,
//! genuine complete input seal and source proof, actual owner transaction.
use super::*;
use completion::InternalFixture;
use llm::bounded::CompletionLimits;
use llm::{EgressFirewall, EgressPolicy, OllamaProvider};
use serde_json::json;
use std::time::Duration;

struct Fixture {
    f: InternalFixture,
    target: InvestigationInspectionTarget,
    current: InvestigationInput,
    prepared: PreparedBoundedCompletion,
    proof: UnreadSourcePreviewBound,
    response: InvestigationResponseMeta,
    action: Value,
    active: u64,
}
impl Fixture {
    fn new() -> Self {
        let (f, target) = source_validation::internal_source_invocation_fixture();
        let raw=json!({"context":{"context_id":f.request.expected_context_id,
            "coverage":f.ledger.coverage.as_ref().unwrap().qualification,
            "response":{"snapshot_id":f.ledger.scope_snapshot_id,"view":"recovered_graph","total_selected":1,
            "facts":[{"reference":target.fact,"properties":{"note":"bounded native fixture"}}],"next_cursor":null}},
            "evidence_options":[{"fact":target.fact,"origin":"captured_primary_source_metadata","ranges":[]}]}).to_string();
        assert_eq!(
            core_prov::content_hash(raw.as_bytes()),
            f.ledger.queries[0].response_hash
        );
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let basis = turn::basis(&tx, &f.task).unwrap();
        tx.commit().unwrap();
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
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
        let provider = OllamaProvider::new(
            "http://127.0.0.1:11434",
            "test-model",
            Duration::from_secs(1),
        )
        .unwrap();
        let prepared = EgressFirewall::new(EgressPolicy::default())
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
                .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
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
                &INVESTIGATION_V2_POLICY,
            )
            .unwrap();
        let action = json!({"type":"read_evidence","fact":target.fact,"role":target.role,"index":target.index});
        let response = InvestigationResponseMeta {
            step_id: f.step.step_id.clone(),
            response_hash: core_prov::content_hash(b"actual complete source action response"),
            action_hash: core_prov::content_hash(&encode(&action, MAX_ACTION_BYTES).unwrap()),
            observed_model: None,
            reported_input_tokens: Some(17),
            reported_output_tokens: Some(9),
            active_milliseconds: f.task.detail.usage.active_milliseconds + 2,
        };
        let active = response.active_milliseconds + 5;
        Self {
            f,
            target,
            current,
            prepared,
            proof,
            response,
            action,
            active,
        }
    }
    fn publish(&mut self) -> Result<CommittedSourceReservation, InvestigationStoreError> {
        self.f.store.admit_next_prompt_source(
            &self.f._execution_guard,
            self.f.task.detail.summary.revision,
            &SourceAdmission {
                response: &self.response,
                original_action: &self.action,
                current: &self.current,
                current_prepared: &self.prepared,
                proof: &self.proof,
                target: &self.target,
                active_milliseconds: self.active,
            },
            |_| Ok(()),
        )
    }
    fn snapshot(&self) -> Vec<Vec<u8>> {
        let tx = self.f.store.conn.unchecked_transaction().unwrap();
        let saved = load_next_prompt_task_base(&tx, &self.f.basis.task_id).unwrap();
        let mut result = vec![encode(&saved, MAX_RECORD_BYTES).unwrap()];
        let mut statement = tx
            .prepare("SELECT name FROM sqlite_schema ORDER BY name")
            .unwrap();
        result.extend(
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .map(|row| row.unwrap().into_bytes()),
        );
        for table in [
            "investigation_responses",
            "investigation_events",
            "investigation_inputs",
            "investigation_invocations",
        ] {
            let mut statement = tx
                .prepare(&format!(
                    "SELECT body FROM {table} WHERE task_id=?1 ORDER BY body"
                ))
                .unwrap();
            result.extend(
                statement
                    .query_map([&self.f.basis.task_id], |row| row.get::<_, Vec<u8>>(0))
                    .unwrap()
                    .map(Result::unwrap),
            );
        }
        result
    }
}

#[test]
fn source_admission_atomic_commit_binds_exact_response_proof_claims_and_no_read_debit() {
    let mut f = Fixture::new();
    let prior = f.f.task.detail.usage.clone();
    let capability = f.publish().unwrap();
    assert!(capability.matches(&f.target, &f.f.basis.input_ledger_hash));
    assert_eq!(
        capability.detail().usage.model_invocations,
        prior.model_invocations
    );
    assert_eq!(capability.detail().usage.tool_actions, prior.tool_actions);
    assert_eq!(
        capability.detail().usage.evidence_requests,
        prior.evidence_requests
    );
    assert_eq!(
        capability.detail().usage.evidence_items,
        prior.evidence_items
    );
    assert_eq!(
        capability.detail().usage.evidence_bytes,
        prior.evidence_bytes
    );
    assert_eq!(
        capability.detail().usage.captured_validation_bytes,
        prior.captured_validation_bytes
    );
    assert_eq!(
        capability.detail().usage.active_milliseconds,
        f.response.active_milliseconds
    );
    let reopened = JobStore::open(f.f.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let saved = load_next_prompt_task_base(&tx, &f.f.basis.task_id).unwrap();
    assert_eq!(saved.phase, Phase::Reservation);
    assert!(saved.pending.is_none());
    let reservation = source_validation::validate_saved_pending(&tx, &saved).unwrap();
    assert!(reservation.charge.is_none());
    assert!(reservation.disposition.is_none());
    assert_eq!(
        reservation.reservation.binding.certification_basis_hash,
        f.proof.basis_hash()
    );
    assert_eq!(
        reservation.reservation.binding.original_action_hash,
        f.response.action_hash
    );
    let envelope =
        source_settlement::envelope(&tx, &saved, &reservation.reservation, &f.response).unwrap();
    assert_eq!(
        reservation.reservation.storage_claim.validation_bytes,
        source_validation::validation_storage_bound(&tx, &saved, &reservation.reservation).unwrap()
    );
    assert_eq!(
        reservation.reservation.storage_claim.settlement_bytes,
        envelope.bytes
    );
    assert_eq!(
        reservation.reservation.storage_claim.settlement_attempts,
        envelope.attempts
    );
    assert_eq!(
        capability.basis().reservation_hash,
        reservation.reservation.fingerprint().unwrap()
    );
    tx.commit().unwrap();
    let charged =
        f.f.store
            .charge_prepared_source_validation(
                &f.f._execution_guard,
                &capability.basis(),
                &f.target,
                &f.f.basis.input_ledger_hash,
            )
            .unwrap();
    assert_eq!(
        charged.detail().usage.captured_validation_bytes,
        prior.captured_validation_bytes + f.target.captured.file.byte_len
    );
    assert_eq!(
        charged.detail().usage.evidence_requests,
        prior.evidence_requests
    );
}

#[test]
fn source_admission_rebound_action_target_proof_or_callback_leaves_no_partial_admission() {
    let mut f = Fixture::new();
    let before = f.snapshot();
    f.action["index"] = json!(1);
    assert!(f.publish().is_err());
    assert_eq!(f.snapshot(), before);
    f.action["index"] = json!(0);
    let original = f.target.clone();
    f.target.source.path = "other.ts".into();
    f.target.captured.file.path = "other.ts".into();
    f.target.target_id = f.target.content_id().unwrap();
    assert!(f.publish().is_err());
    assert_eq!(f.snapshot(), before);
    f.target = original;
    f.active = f.f.task.detail.limits.active_seconds * 1000;
    assert!(f.publish().is_err());
    assert_eq!(f.snapshot(), before);
    f.active = f.response.active_milliseconds + 5;
    let called = std::cell::Cell::new(0);
    let result = f.f.store.admit_next_prompt_source(
        &f.f._execution_guard,
        f.f.task.detail.summary.revision,
        &SourceAdmission {
            response: &f.response,
            original_action: &f.action,
            current: &f.current,
            current_prepared: &f.prepared,
            proof: &f.proof,
            target: &f.target,
            active_milliseconds: f.active,
        },
        |_| {
            called.set(called.get() + 1);
            Err(InvestigationStoreError::Cancelled)
        },
    );
    assert!(matches!(result, Err(InvestigationStoreError::Cancelled)));
    assert_eq!(called.get(), 1);
    assert_eq!(f.snapshot(), before);
}

#[test]
fn source_admission_final_write_rollback_and_lost_ack_return_no_false_capability() {
    let mut f = Fixture::new();
    let before = f.snapshot();
    FAIL_BEFORE_SOURCE_ADMISSION_COMMIT.with(|flag| flag.set(true));
    assert!(matches!(
        f.publish(),
        Err(InvestigationStoreError::Capacity)
    ));
    assert_eq!(f.snapshot(), before);
    LOSE_SOURCE_ADMISSION_ACK.with(|flag| flag.set(true));
    assert!(matches!(f.publish(), Err(InvestigationStoreError::Storage)));
    let committed = f.snapshot();
    assert_ne!(committed, before);
    assert!(matches!(f.publish(), Err(InvestigationStoreError::Stale)));
    assert_eq!(f.snapshot(), committed);
    let reopened = JobStore::open(f.f.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &f.f.basis.task_id).unwrap();
    assert!(
        source_validation::validate_saved_pending(&tx, &task)
            .unwrap()
            .charge
            .is_none()
    );
    assert_eq!(task.detail.usage.captured_validation_bytes, 0);
}

#[test]
fn source_admission_count_headroom_keeps_exact_terminal_and_attempt_slots() {
    let mut f = Fixture::new();
    f.publish().unwrap();
    let tx = f.f.store.conn.unchecked_transaction().unwrap();
    let mut task = load_next_prompt_task_base(&tx, &f.f.basis.task_id).unwrap();
    let saved = source_validation::validate_saved_pending(&tx, &task).unwrap();
    let envelope =
        source_settlement::envelope(&tx, &task, &saved.reservation, &f.response).unwrap();
    let policy = &INVESTIGATION_V2_POLICY;
    task.detail.summary.last_event_sequence =
        policy.journal_events() - saved.reservation.storage_claim.events(false) as u64 - 3;
    count_headroom(
        &tx,
        &task,
        &saved.reservation,
        &envelope,
        policy.max_completed_attempts() - envelope.attempts,
    )
    .unwrap();
    task.detail.summary.last_event_sequence += 1;
    assert_eq!(
        count_headroom(&tx, &task, &saved.reservation, &envelope, 0),
        Err(InvestigationStoreError::Capacity)
    );
    task.detail.summary.last_event_sequence -= 1;
    assert_eq!(
        count_headroom(
            &tx,
            &task,
            &saved.reservation,
            &envelope,
            policy.max_completed_attempts() - envelope.attempts + 1
        ),
        Err(InvestigationStoreError::Capacity)
    );
}

#[test]
fn source_admission_real_cancellation_and_foreign_execution_cannot_create_reservation() {
    let mut f = Fixture::new();
    let other = Fixture::new();
    let before = f.snapshot();
    let input = SourceAdmission {
        response: &f.response,
        original_action: &f.action,
        current: &f.current,
        current_prepared: &f.prepared,
        proof: &f.proof,
        target: &f.target,
        active_milliseconds: f.active,
    };
    assert!(matches!(
        f.f.store.admit_next_prompt_source(
            &other.f._execution_guard,
            f.f.task.detail.summary.revision,
            &input,
            |_| Ok(())
        ),
        Err(InvestigationStoreError::Ownership)
    ));
    assert_eq!(f.snapshot(), before);
    f.f.store
        .cancel_next_prompt_investigation(&f.f.basis.task_id)
        .unwrap();
    {
        let tx = f.f.store.conn.unchecked_transaction().unwrap();
        f.f.task = load_next_prompt_task_base(&tx, &f.f.basis.task_id).unwrap();
    }
    let cancelled = f.snapshot();
    assert!(matches!(
        f.publish(),
        Err(InvestigationStoreError::Cancelled)
    ));
    assert_eq!(f.snapshot(), cancelled);
}
