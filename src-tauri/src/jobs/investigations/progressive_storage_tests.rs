use super::*;

fn progressive() -> Fixture {
    Fixture::with_family(
        InvestigationProviderMode::Local,
        &INVESTIGATION_V2_POLICY,
        true,
    )
}

#[test]
fn progressive_storage_new_intent_reopens_and_cannot_alias_older_family_nonce() {
    // AC-0286 / T-0286: request6 has its own durable family and saved identity.
    let mut fixture = progressive();
    let detail = fixture.store.investigation(&fixture.id).unwrap();
    assert_eq!(detail.summary.schema_version, 6);
    assert_eq!(detail.summary.specialist_id, SpecialistId::DomainAnalystV9);
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    let task = load_task_base(&tx, &fixture.id).unwrap();
    assert_eq!(task.completion_intent.as_ref().unwrap().schema_version, 3);
    tx.commit().unwrap();
    let same = fixture
        .store
        .start_completion_investigation(&fixture.request, None)
        .unwrap();
    assert!(!same.created);
    assert_eq!(same.detail, detail);
    let mut older = fixture.request.clone();
    older.schema_version = 5;
    older.specialist_id = "domain-analyst@8".into();
    assert!(matches!(
        fixture.store.existing_investigation_completion(&older),
        Err(InvestigationStoreError::Conflict)
    ));
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
}

#[test]
fn progressive_storage_empty_history_binding_must_match_actual_saved_request() {
    // AC-0286 / T-0286: even a valid empty registry cannot rebind task authority.
    let mut fixture = progressive();
    let (_, step) = fixture.prepare_step();
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    let task = load_task_base(&tx, &fixture.id).unwrap();
    let ledger = load_ledger(&tx, &task).unwrap().unwrap();
    assert!(ledger.queries.is_empty());
    assert_eq!(step.disclosure.as_ref().unwrap().schema_version, 5);
    validate_step_metadata(&tx, &task, &step).unwrap();
    let other_hash = "a".repeat(64);
    assert_ne!(other_hash, task.intent_hash);
    let projection = ProgressiveInvestigationContext::prepare(
        &other_hash,
        &ledger,
        &[],
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    let mut rebound = step.clone();
    let binding = rebound
        .disclosure
        .as_mut()
        .unwrap()
        .query_history
        .as_mut()
        .unwrap();
    binding.request_hash = other_hash;
    binding.registry_hash = projection.query_history.registry_hash;
    rebound
        .disclosure
        .as_ref()
        .unwrap()
        .validate_with_policy(&ledger, &INVESTIGATION_V2_POLICY)
        .unwrap();
    assert!(validate_step_metadata(&tx, &task, &rebound).is_err());
    tx.commit().unwrap();
    assert!(
        fixture
            .advance(InvestigationTransition::BeginInvocation { step: rebound })
            .is_err()
    );
    let after = fixture.store.investigation(&fixture.id).unwrap();
    assert_eq!(after.usage.model_invocations, 0);
    assert_eq!(after.usage.generated_token_reservations, 0);
    fixture
        .advance(InvestigationTransition::BeginInvocation { step })
        .unwrap();
}

#[test]
fn progressive_storage_exact_family_rejects_legacy_intent_and_disclosure_versions() {
    // AC-0286 / T-0286: numeric V2 equality does not merge immutable protocols.
    let mut fixture = progressive();
    let (_, step) = fixture.prepare_step();
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    let task = load_task_base(&tx, &fixture.id).unwrap();
    let mut legacy = load_task_base(&tx, &fixture.id).unwrap();
    legacy.completion_intent.as_mut().unwrap().schema_version = 2;
    assert!(
        legacy
            .completion_intent
            .as_ref()
            .unwrap()
            .validate(&legacy)
            .is_err()
    );
    let mut old_step = step.clone();
    old_step.disclosure.as_mut().unwrap().schema_version = 4;
    old_step.disclosure.as_mut().unwrap().query_history = None;
    assert!(validate_step_metadata(&tx, &task, &old_step).is_err());
    tx.commit().unwrap();
    fixture
        .advance(InvestigationTransition::BeginInvocation { step })
        .unwrap();
    // An admitted disclosure5 remains evidence even if mutable metadata is erased.
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    assert!(validate_legacy_evidence(&tx, &task).is_err());
    tx.commit().unwrap();
}

#[test]
fn progressive_summary_reader_rejects_unactivated_admission_generation() {
    // AC-0287: recognizing an internal specialist cannot admit a future saved UI record.
    let fixture = progressive();
    let before = fixture.store.investigation(&fixture.id).unwrap();
    for summary_schema in [6, 7] {
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        let mut task = load_task_base(&tx, &fixture.id).unwrap();
        task.detail.summary.schema_version = summary_schema;
        task.detail.specialist = SpecialistId::DomainAnalystV10.definition().unwrap();
        task.detail.summary.specialist_id = SpecialistId::DomainAnalystV10;
        let bytes = serde_json::to_vec(&task).unwrap();
        tx.execute(
            "UPDATE investigation_tasks SET record=?1, record_hash=?2 WHERE id=?3",
            rusqlite::params![bytes, core_prov::content_hash(&bytes), fixture.id],
        )
        .unwrap();
        assert!(load_task_base(&tx, &fixture.id).is_err());
        tx.rollback().unwrap();
    }
    assert_eq!(fixture.store.investigation(&fixture.id).unwrap(), before);
}

#[test]
fn progressive_internal_start_cannot_persist_admission_role_as_old_task_family() {
    // AC-0287: internal normalization alone must not route @10 into task2/summary6.
    let mut fixture = progressive();
    let mut future = fixture.request.clone();
    future.schema_version = 7;
    future.specialist_id = "domain-analyst@10".into();
    future.request_nonce = "unactivated-admission-start".into();
    future
        .normalized_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let before: i64 = fixture
        .store
        .conn
        .query_row("SELECT count(*) FROM investigation_tasks", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        fixture
            .store
            .start_completion_investigation(&future, None)
            .is_err()
    );
    let after: i64 = fixture
        .store
        .conn
        .query_row("SELECT count(*) FROM investigation_tasks", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(before, after);
}
