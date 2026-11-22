//! AC-0287: final-write rollback and lost commit acknowledgement. Outcomes are
//! scripted native fixtures; no provider or actual source read is executed.
use super::*;

#[test]
fn source_settlement_final_save_failure_rolls_back_every_publication_effect() {
    let mut fixture = Fixture::new();
    fixture.charge();
    let (candidate, prepared) = fixture.candidate(None);
    let before = encode(&fixture.f.task, MAX_RECORD_BYTES).unwrap();
    let occupied = {
        let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
        stored_bytes(&tx, Some(&fixture.f.basis.task_id)).unwrap()
    };
    storage_writer::fail_once(storage_writer::SettlementFailure::AfterTaskSave);
    let c = SourceContinuation {
        size_binding: None,
        current: &fixture.current,
        current_prepared: &fixture.prepared,
        candidate: &candidate,
        candidate_prepared: &prepared,
        proof: &fixture.proof,
        refusal: None,
    };
    let result = fixture.f.store.settle_next_prompt_source(
        &fixture.f._execution_guard,
        fixture.f.task.detail.summary.revision,
        SourceSettlement::Acquired(c),
        fixture.f.task.detail.usage.active_milliseconds + 1,
        |_| Ok(()),
    );
    assert_eq!(result.unwrap_err(), InvestigationStoreError::Storage);
    let reopened = JobStore::open(fixture.f.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &fixture.f.basis.task_id).unwrap();
    assert_eq!(encode(&task, MAX_RECORD_BYTES).unwrap(), before);
    assert_eq!(task.phase, Phase::Reservation);
    assert_eq!(
        stored_bytes(&tx, Some(&fixture.f.basis.task_id)).unwrap(),
        occupied
    );
    let saved = read_reservations::load(&tx, &fixture.f.basis.task_id, &fixture.f.step.step_id)
        .unwrap()
        .unwrap();
    assert!(saved.charge.is_some());
    assert!(saved.disposition.is_none());
    assert!(completion::attempts(&tx, &task).unwrap().is_empty());
    assert_eq!(
        tx.query_row::<i64, _, _>(
            "SELECT count(*) FROM investigation_inputs WHERE task_id=?1",
            [&fixture.f.basis.task_id],
            |r| r.get(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        tx.query_row::<i64, _, _>(
            "SELECT count(*) FROM investigation_events WHERE task_id=?1",
            [&fixture.f.basis.task_id],
            |r| r.get(0)
        )
        .unwrap(),
        signed(task.detail.summary.last_event_sequence).unwrap()
    );
}

#[test]
fn source_settlement_lost_commit_ack_retains_one_acquisition_and_refuses_same_step_replay() {
    let mut fixture = Fixture::new();
    fixture.charge();
    let (candidate, prepared) = fixture.candidate(None);
    let revision = fixture.f.task.detail.summary.revision;
    let active = fixture.f.task.detail.usage.active_milliseconds + 1;
    let retained_checks = std::cell::Cell::new(0);
    storage_writer::fail_once(storage_writer::SettlementFailure::AfterCommitAck);
    let c = SourceContinuation {
        size_binding: None,
        current: &fixture.current,
        current_prepared: &fixture.prepared,
        candidate: &candidate,
        candidate_prepared: &prepared,
        proof: &fixture.proof,
        refusal: None,
    };
    let result = fixture.f.store.settle_next_prompt_source(
        &fixture.f._execution_guard,
        revision,
        SourceSettlement::Acquired(c),
        active,
        |_| {
            retained_checks.set(retained_checks.get() + 1);
            Ok(())
        },
    );
    assert_eq!(result.unwrap_err(), InvestigationStoreError::Storage);
    assert_eq!(retained_checks.get(), 1);
    let reopened = JobStore::open(fixture.f.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &fixture.f.basis.task_id).unwrap();
    assert_eq!(task.phase, Phase::Ready);
    assert_eq!(task.detail.usage.evidence_items, 1);
    assert_eq!(task.detail.usage.captured_validation_bytes, 12);
    assert_eq!(completion::attempts(&tx, &task).unwrap().len(), 1);
    let saved = read_reservations::load(&tx, &fixture.f.basis.task_id, &fixture.f.step.step_id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.disposition.unwrap().kind, DispositionKind::Acquired);
    assert_eq!(
        read_reservations::pending_storage_claims(&tx, Some(&fixture.f.basis.task_id)).unwrap(),
        0
    );
    drop(tx);
    let c = SourceContinuation {
        size_binding: None,
        current: &fixture.current,
        current_prepared: &fixture.prepared,
        candidate: &candidate,
        candidate_prepared: &prepared,
        proof: &fixture.proof,
        refusal: None,
    };
    assert_eq!(
        fixture
            .f
            .store
            .settle_next_prompt_source(
                &fixture.f._execution_guard,
                revision,
                SourceSettlement::Acquired(c),
                active,
                |_| {
                    retained_checks.set(retained_checks.get() + 1);
                    Ok(())
                }
            )
            .unwrap_err(),
        InvestigationStoreError::Stale
    );
    assert_eq!(retained_checks.get(), 1);
}
