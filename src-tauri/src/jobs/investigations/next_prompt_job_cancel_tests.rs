//! AC-0287: generic Jobs cancellation authenticates the stored family and
//! retains the real execution owner and all pending read allocations.
use super::*;

fn cancel_and_check(f: &mut InitialFixture) {
    let id = f.task.detail.summary.investigation_id.clone();
    let job = f.task.detail.summary.job_id;
    let before = f.task.detail.clone();
    let before_phase = f.task.phase;
    let before_ledger = f.store.investigation_ledger(&id).unwrap();
    let owner = candidate(f);
    assert_eq!(f.store.cancel(job).unwrap().status, "cancelled");
    let detail = f.store.investigation(&id).unwrap();
    assert!(detail.summary.cancel_requested);
    assert_eq!(detail.summary.revision, before.summary.revision + 1);
    assert_eq!(
        detail.summary.last_event_sequence,
        before.summary.last_event_sequence + 1
    );
    assert_eq!(detail.summary.status, before.summary.status);
    assert_eq!(detail.usage, before.usage);
    assert_eq!(f.store.investigation_ledger(&id).unwrap(), before_ledger);
    let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&id).unwrap(), detail);
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_execution_task(&tx, &id).unwrap();
    assert_eq!(task.phase, before_phase);
    assert_eq!(task.pending, f.task.pending);
    assert_eq!(task.pending_source_read, f.task.pending_source_read);
    assert_eq!(
        last_event(&tx, &id, detail.summary.last_event_sequence).kind,
        InvestigationEventKind::CancelRequested
    );
    let live: i64 = tx
        .query_row(
            "SELECT live FROM investigation_tasks WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(live, 1);
    tx.commit().unwrap();
    let locks = JobExecutionLocks::open(f.directory.path(), f.store.execution_namespace()).unwrap();
    assert!(matches!(
        locks.try_reserve(owner.lock_target()),
        Err(JobTransitionError::Busy)
    ));

    // Keep generic Jobs' existing second-cancel contract, while the internal
    // cancellation hook itself is idempotent and adds no duplicate event.
    let after = saved(f);
    assert!(matches!(
        f.store.cancel(job),
        Err(JobTransitionError::InvalidFrom { verb: "cancel", .. })
    ));
    let tx = f.store.conn.unchecked_transaction().unwrap();
    cancel_for_job(&tx, &f.store.execution_namespace(), job).unwrap();
    tx.commit().unwrap();
    assert_eq!(saved(f), after);
}

#[test]
fn next_prompt_job_cancel_ready_and_invocation_reopen_without_refunds_or_owner_release() {
    for invoked in [false, true] {
        let mut f = Fixture::new(false);
        if invoked {
            f.begin();
        }
        cancel_and_check(&mut f.f);
    }
}

#[test]
fn next_prompt_job_cancel_pending_source_keeps_claim_and_validation_charge() {
    for charged in [false, true] {
        let mut f = fixture::source(charged);
        let id = f.task.detail.summary.investigation_id.clone();
        let step = f.task.pending_source_read.as_ref().unwrap().step_id.clone();
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let before = read_reservations::load(&tx, &id, &step).unwrap().unwrap();
        let claims = read_reservations::pending_storage_claims(&tx, Some(&id)).unwrap();
        assert!(claims > 0);
        assert_eq!(before.charge.is_some(), charged);
        tx.commit().unwrap();
        cancel_and_check(&mut f);
        let tx = f.store.conn.unchecked_transaction().unwrap();
        assert_eq!(
            read_reservations::load(&tx, &id, &step).unwrap().as_ref(),
            Some(&before)
        );
        assert_eq!(
            read_reservations::pending_storage_claims(&tx, Some(&id)).unwrap(),
            claims
        );
        tx.commit().unwrap();
        // Cancellation must not prevent the real orphan boundary from later
        // settling unknown, with exactly the same validation charge retained.
        fixture::refresh(&mut f);
        let before_usage = f.task.detail.usage.clone();
        let owner = candidate(&f);
        let (_directory, store, _, detail) = recover(f, owner);
        assert_eq!(detail.summary.status, InvestigationStatus::OutcomeUnknown);
        assert_eq!(detail.usage, before_usage);
        let tx = store.conn.unchecked_transaction().unwrap();
        let settled = read_reservations::load(&tx, &id, &step).unwrap().unwrap();
        assert_eq!(settled.charge, before.charge);
        let disposition = settled.disposition.unwrap();
        assert_eq!(disposition.kind, DispositionKind::OutcomeUnknown);
        assert_eq!(disposition.accounting.reserved_unknown_tool_actions, 1);
        assert_eq!(disposition.accounting.reserved_unknown_evidence_requests, 1);
    }
}

#[test]
fn next_prompt_job_cancel_corrupt_task_or_read_journal_rolls_back_job_status() {
    for journal in [false, true] {
        let mut f = if journal {
            fixture::source(true)
        } else {
            Fixture::new(false).f
        };
        let id = f.task.detail.summary.investigation_id.clone();
        let job = f.task.detail.summary.job_id;
        if journal {
            f.store
                .conn
                .execute_batch("DROP TRIGGER investigation_read_reservations_no_update")
                .unwrap();
            f.store
                .conn
                .execute(
                    "UPDATE investigation_read_reservations SET body_hash=?2 WHERE task_id=?1",
                    params![id, "0".repeat(64)],
                )
                .unwrap();
            f.store.conn.execute_batch("CREATE TRIGGER investigation_read_reservations_no_update BEFORE UPDATE ON investigation_read_reservations BEGIN SELECT RAISE(ABORT, 'immutable investigation read record'); END").unwrap();
        } else {
            f.store
                .conn
                .execute(
                    "UPDATE investigation_tasks SET record_hash=?2 WHERE id=?1",
                    params![id, "0".repeat(64)],
                )
                .unwrap();
        }
        let before = saved(&f);
        assert!(matches!(
            f.store.cancel(job),
            Err(JobTransitionError::InvalidMetadata)
        ));
        assert_eq!(f.store.get(job).unwrap().status, "running");
        assert_eq!(saved(&f), before);
    }
}
