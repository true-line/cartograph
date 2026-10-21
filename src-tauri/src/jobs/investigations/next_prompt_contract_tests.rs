use super::*;
use crate::jobs::investigations::admission_contract::{self, SavedCallBasis, StoredAdmissionState};
use agents::investigation::staged_refusal::CompletionCallMode;

/// Private test construction only. Public starts/history still reject @10.
/// Uses a real existing job/ledger/archive fixture, then explicitly writes the
/// new internal DTO/invocation as test data; this is not a production migration.
pub(in crate::jobs::investigations) struct InternalFixture {
    pub directory: tempfile::TempDir,
    pub store: JobStore,
    pub task: StoredTask,
    pub request: StagedObligationRequest,
    pub ledger: InvestigationInputLedger,
    pub step: PendingInvestigationStep,
    pub metadata: ProtectedCompletionMetadata,
    pub basis: SavedCallBasis,
    pub(in crate::jobs::investigations) _execution_guard: JobExecution,
}
fn clone_task(task: &StoredTask) -> StoredTask {
    serde_json::from_slice(&encode(task, MAX_RECORD_BYTES).unwrap()).unwrap()
}
fn prepare(
    task: &StoredTask,
    request: &StagedObligationRequest,
    ledger: &InvestigationInputLedger,
    metadata: &ProtectedCompletionMetadata,
) -> PendingInvestigationStep {
    prepare_with_pages(task, request, ledger, metadata, &[])
}
fn prepare_with_pages(
    task: &StoredTask,
    request: &StagedObligationRequest,
    ledger: &InvestigationInputLedger,
    metadata: &ProtectedCompletionMetadata,
    pages: &[String],
) -> PendingInvestigationStep {
    let policy = &INVESTIGATION_V2_POLICY;
    let turn = InvestigationTurnBasis {
        input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
        previous_ledger: None,
        completed_tool: None,
    };
    let mode = metadata
        .next_call_mode_with_policy(&task.detail.usage, policy)
        .unwrap();
    let input = InvestigationInput::prepare_v9_with_policy(
        &request.question,
        task.detail.summary.specialist_id,
        ledger.clone(),
        &pages.iter().map(String::as_str).collect::<Vec<_>>(),
        &[],
        &[],
        &task.detail.usage,
        &turn,
        request,
        metadata,
        &[],
        mode,
        0,
        None,
        policy,
    )
    .unwrap();
    let step_id = format!("admission-step:{}", task.detail.usage.model_invocations + 1);
    let action = input.action(step_id.clone()).unwrap();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let prepared = EgressFirewall::new(EgressPolicy::default())
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    let disclosure = input
        .seal_disclosure_with_policy(&prepared, policy)
        .unwrap()
        .unwrap();
    PendingInvestigationStep {
        step_id,
        payload_hash: prepared.preview().egress.payload_hash.clone(),
        input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
        input_bytes: serde_json::to_vec(&action.payload).unwrap().len(),
        generated_tokens: policy.limits().generated_tokens_per_invocation,
        disclosure: Some(disclosure),
    }
}

pub(in crate::jobs::investigations) fn internal_fixture() -> InternalFixture {
    internal_fixture_with_initial_ledger(|_, _, _, _| vec![])
}

pub(in crate::jobs::investigations) fn internal_fixture_with_initial_ledger(
    initialize: impl FnOnce(
        &mut JobStore,
        &mut StoredTask,
        &mut InvestigationInputLedger,
        &mut StagedObligationRequest,
    ) -> Vec<String>,
) -> InternalFixture {
    let mut fixture = Fixture::with_family(
        InvestigationProviderMode::Local,
        &INVESTIGATION_V2_POLICY,
        true,
    );
    let mut ledger = fixture
        .store
        .investigation_ledger(&fixture.id)
        .unwrap()
        .unwrap();
    let mut request = fixture.request.clone();
    request.schema_version = 7;
    request.specialist_id = "domain-analyst@10".into();
    let mut task = {
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        load_task(&tx, &fixture.id).unwrap()
    };
    task.schema_version = 3;
    task.admission_state = Some(StoredAdmissionState {
        schema_version: 1,
        current_call: None,
    });
    task.detail.summary.schema_version = 7;
    task.detail.summary.specialist_id = SpecialistId::DomainAnalystV10;
    task.detail.specialist = SpecialistId::DomainAnalystV10.definition().unwrap();
    let pages = initialize(&mut fixture.store, &mut task, &mut ledger, &mut request);
    let request = request
        .normalized_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let intent = CompletionIntent::new(&request).unwrap();
    task.intent_hash = intent.request_hash.clone();
    task.detail.completion = Some(initial_detail(&intent));
    task.completion_intent = Some(intent);
    let metadata = derive_protected_metadata(&task, vec![], &[]).unwrap();
    let step = prepare_with_pages(&task, &request, &ledger, &metadata, &pages);
    let basis = {
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        SavedCallBasis::from_pre_call_saved(&tx, &task, &step, &ledger, &metadata).unwrap()
    };
    task.admission_state.as_mut().unwrap().current_call = Some(basis.clone());
    task.phase = Phase::Invocation;
    task.pending = Some(step.clone());
    task.detail.usage.model_invocations += 1;
    task.detail.usage.generated_token_reservations += step.generated_tokens;
    {
        let tx = fixture.store.conn.transaction().unwrap();
        insert_body(
            &tx,
            "investigation_invocations",
            &fixture.id,
            &step.step_id,
            &step,
            MAX_INVOCATION_BYTES,
        )
        .unwrap();
        bump(&tx, &mut task).unwrap();
        event(
            &tx,
            &mut task,
            InvestigationEventKind::ModelStarted,
            Some(&step.step_id),
            None,
            "Internal test invocation boundary; no provider executed.",
        )
        .unwrap();
        validate_next_prompt_task(&task).unwrap();
        let bytes = encode(&task, MAX_RECORD_BYTES).unwrap();
        tx.execute(
            "UPDATE investigation_tasks SET revision=?2,record=?3,record_hash=?4 WHERE id=?1",
            params![
                fixture.id,
                signed(task.detail.summary.revision).unwrap(),
                bytes,
                core_prov::content_hash(&bytes)
            ],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    InternalFixture {
        directory: fixture.directory,
        store: fixture.store,
        task,
        request,
        ledger,
        step,
        metadata,
        basis,
        _execution_guard: fixture.execution,
    }
}

#[test]
fn next_prompt_contract_exact_task3_intent4_pairing_keeps_public_history_closed() {
    // AC-0287: explicit new private DTO family; no old row/default migration.
    let fixture = internal_fixture();
    validate_next_prompt_task(&fixture.task).unwrap();
    assert_eq!(
        save_next_prompt_task(
            &fixture.store.conn,
            &fixture.task,
            fixture.task.detail.summary.revision,
            false
        ),
        Err(InvestigationStoreError::Invalid)
    );
    assert_eq!(
        fixture
            .task
            .completion_intent
            .as_ref()
            .unwrap()
            .schema_version,
        4
    );
    assert_eq!(
        fixture
            .task
            .completion_intent
            .as_ref()
            .unwrap()
            .request_schema_version,
        Some(7)
    );
    let id = &fixture.task.detail.summary.investigation_id;
    assert!(fixture.store.investigation(id).is_err());
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert!(reopened.investigation(id).is_err());
    for change in [
        |t: &mut StoredTask| t.schema_version = 2,
        |t: &mut StoredTask| t.detail.summary.schema_version = 6,
        |t: &mut StoredTask| t.detail.summary.specialist_id = SpecialistId::DomainAnalystV9,
        |t: &mut StoredTask| t.completion_intent.as_mut().unwrap().schema_version = 3,
        |t: &mut StoredTask| t.completion_intent.as_mut().unwrap().request_schema_version = Some(6),
        |t: &mut StoredTask| t.admission_state = None,
        |t: &mut StoredTask| t.admission_state.as_mut().unwrap().current_call = None,
    ] {
        let mut bad = clone_task(&fixture.task);
        change(&mut bad);
        assert!(validate_next_prompt_task(&bad).is_err());
    }
}

#[test]
fn next_prompt_contract_pre_call_mode_survives_charge_and_serialized_reopen() {
    // AC-0287: Choice at61 becomes FinishOnly at62 if wrongly recomputed;
    // FinishOnly at62 has no reserve at63. Persisted preusage preserves both.
    let fixture = internal_fixture();
    for (calls, expected) in [
        (61, CompletionCallMode::Choice),
        (62, CompletionCallMode::FinishOnly),
    ] {
        let mut task = clone_task(&fixture.task);
        task.phase = Phase::Ready;
        task.pending = None;
        task.detail.summary.invocation_pending = false;
        task.admission_state.as_mut().unwrap().current_call = None;
        task.detail.usage.model_invocations = calls;
        task.detail.usage.generated_token_reservations = calls * 2048;
        let step = prepare(&task, &fixture.request, &fixture.ledger, &fixture.metadata);
        let basis = SavedCallBasis::from_pre_call(
            &task,
            &step,
            &fixture.ledger,
            &fixture.metadata,
            &admission_journal::CapacityState {
                count: 0,
                feedback: None,
            },
        )
        .unwrap();
        assert_eq!(basis.mode(), expected);
        task.phase = Phase::Invocation;
        task.pending = Some(step.clone());
        task.detail.summary.invocation_pending = true;
        task.admission_state.as_mut().unwrap().current_call = Some(basis.clone());
        task.detail.usage.model_invocations += 1;
        task.detail.usage.generated_token_reservations += step.generated_tokens;
        let reopened = clone_task(&task);
        validate_next_prompt_task(&reopened).unwrap();
        basis
            .validate_core(&reopened, &step, &fixture.ledger, &fixture.metadata)
            .unwrap();
        basis.validate_charged_usage(&reopened, &step).unwrap();
        assert_ne!(
            fixture
                .metadata
                .next_call_mode_with_policy(&reopened.detail.usage, &INVESTIGATION_V2_POLICY)
                .ok(),
            Some(expected)
        );
        assert_eq!(
            reopened
                .admission_state
                .unwrap()
                .current_call
                .unwrap()
                .mode(),
            expected
        );
    }
}

#[test]
fn next_prompt_contract_saved_call_requires_exact_step_ledger_request_metadata_and_ordinal() {
    let fixture = internal_fixture();
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    fixture.basis.validate_saved(&tx, &fixture.task).unwrap();
    let mut changed_current = clone_task(&fixture.task);
    changed_current
        .admission_state
        .as_mut()
        .unwrap()
        .current_call
        .as_mut()
        .unwrap()
        .completion_metadata_hash = core_prov::content_hash(b"different-current-call");
    assert!(fixture.basis.validate_saved(&tx, &changed_current).is_err());
    let core =
        admission_contract::load_saved_call_basis_core(&tx, &fixture.task, &fixture.basis).unwrap();
    assert_eq!(core.ordinal, 1);
    assert_eq!(core.step, fixture.step);
    assert_eq!(core.ledger, fixture.ledger);
    assert_eq!(core.metadata, fixture.metadata);
    for change in [
        |b: &mut SavedCallBasis| b.step_id = "missing-step".into(),
        |b: &mut SavedCallBasis| b.task_id = "other-task".into(),
        |b: &mut SavedCallBasis| b.request_hash = core_prov::content_hash(b"other-request"),
        |b: &mut SavedCallBasis| b.input_revision = 99,
        |b: &mut SavedCallBasis| b.input_ledger_hash = core_prov::content_hash(b"other-ledger"),
        |b: &mut SavedCallBasis| {
            b.completion_metadata_hash = core_prov::content_hash(b"other-metadata")
        },
        |b: &mut SavedCallBasis| b.invocation_ordinal = 2,
        |b: &mut SavedCallBasis| b.admission.mode = CompletionCallMode::FinishOnly,
    ] {
        let mut bad = fixture.basis.clone();
        change(&mut bad);
        assert!(admission_contract::load_saved_call_basis_core(&tx, &fixture.task, &bad).is_err());
    }
    let mut missing = fixture.step.clone();
    missing.step_id = "missing-step".into();
    assert!(matches!(
        metadata_at_saved_step(&tx, &fixture.task, &missing),
        Err(InvestigationStoreError::Missing)
    ));
    let mut changed = fixture.step.clone();
    changed.payload_hash = core_prov::content_hash(b"different-payload");
    assert!(matches!(
        metadata_at_saved_step(&tx, &fixture.task, &changed),
        Err(InvestigationStoreError::Invalid)
    ));
}

#[test]
fn next_prompt_contract_legacy_task_and_intent_bytes_gain_no_fields() {
    for (policy, progressive) in [
        (&INVESTIGATION_V1_POLICY, false),
        (&INVESTIGATION_V2_POLICY, false),
        (&INVESTIGATION_V2_POLICY, true),
    ] {
        let fixture = Fixture::with_family(InvestigationProviderMode::Local, policy, progressive);
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        let task = load_task(&tx, &fixture.id).unwrap();
        let bytes = encode(&task, MAX_RECORD_BYTES).unwrap();
        let copy: StoredTask = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(encode(&copy, MAX_RECORD_BYTES).unwrap(), bytes);
        let json = serde_json::to_value(&task).unwrap();
        assert!(json.get("admission_state").is_none());
        assert!(
            json["completion_intent"]
                .get("request_schema_version")
                .is_none()
        );
        let mut injected = copy;
        injected.admission_state = Some(StoredAdmissionState {
            schema_version: 1,
            current_call: None,
        });
        let raw = encode(&injected, MAX_RECORD_BYTES).unwrap();
        tx.execute(
            "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
            params![fixture.id, raw, core_prov::content_hash(&raw)],
        )
        .unwrap();
        assert!(load_task_base(&tx, &fixture.id).is_err());
    }
}

#[test]
fn next_prompt_contract_intent4_requires_disclosure6_assessment_basis() {
    let fixture = internal_fixture();
    let intent = fixture.task.completion_intent.as_ref().unwrap();
    for version in [4, 5, 6] {
        let basis = CompletionAssessmentBasis::FinishingDisclosure {
            step_id: fixture.step.step_id.clone(),
            input_ledger_hash: fixture.basis.input_ledger_hash.clone(),
            disclosure_hash: format!("investigation-disclosure-v{version}:{}", "a".repeat(64)),
            finishing_attempts_hash: core_prov::content_hash(b"attempts"),
        };
        assert_eq!(
            intent.validate_assessment_basis(&basis).is_ok(),
            version == 6
        );
    }
}
