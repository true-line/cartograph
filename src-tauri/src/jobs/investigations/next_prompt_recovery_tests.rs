//! AC-0287: real internal starts/claims and lost OS owners, never provider calls.
use super::next_prompt_snapshot::tests::fixture::Fixture;
use super::*;
use crate::job_execution::JobExecutionLocks;
use completion::InitialFixture;
use read_reservations::DispositionKind;
#[path = "next_prompt_recovery_fixture.rs"]
mod fixture;

fn candidate(f: &InitialFixture) -> InvestigationRecoveryCandidate {
    let mut items = f.store.investigation_recovery_candidates().unwrap();
    assert_eq!(items.len(), 1);
    let candidate = items.remove(0);
    let locks = JobExecutionLocks::open(f.directory.path(), f.store.execution_namespace()).unwrap();
    assert!(matches!(
        locks.try_reserve(candidate.lock_target()),
        Err(JobTransitionError::Busy)
    ));
    candidate
}
fn saved(f: &InitialFixture) -> Vec<Vec<u8>> {
    let id = &f.task.detail.summary.investigation_id;
    let mut rows = vec![];
    for (table, column) in [
        ("investigation_tasks", "record"),
        ("investigation_events", "body"),
        ("investigation_invocations", "body"),
        ("investigation_responses", "body"),
        ("investigation_inputs", "body"),
    ] {
        let key = if table == "investigation_tasks" {
            "id"
        } else {
            "task_id"
        };
        rows.extend(
            f.store
                .conn
                .prepare(&format!(
                    "SELECT {column} FROM {table} WHERE {key}=?1 ORDER BY {column}"
                ))
                .unwrap()
                .query_map([id], |r| r.get::<_, Vec<u8>>(0))
                .unwrap()
                .map(Result::unwrap),
        );
    }
    rows
}
fn recover(
    f: InitialFixture,
    candidate: InvestigationRecoveryCandidate,
) -> (tempfile::TempDir, JobStore, String, InvestigationDetail) {
    let InitialFixture {
        directory,
        mut store,
        execution,
        task,
        ..
    } = f;
    let id = task.detail.summary.investigation_id;
    drop(execution);
    let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
    let lock = locks.try_reserve(candidate.lock_target()).unwrap();
    let detail = store
        .recover_investigation(&candidate, lock)
        .unwrap()
        .unwrap();
    assert!(
        store
            .investigation_recovery_candidates()
            .unwrap()
            .is_empty()
    );
    let lock = locks.try_reserve(candidate.lock_target()).unwrap();
    assert!(
        store
            .recover_investigation(&candidate, lock)
            .unwrap()
            .is_none()
    );
    (directory, store, id, detail)
}

#[test]
fn next_prompt_recovery_ready_and_consent_interrupt_without_new_call_or_usage() {
    for cloud in [false, true] {
        let mut f = Fixture::new(cloud);
        if cloud {
            f.f.store
                .await_next_prompt_consent(
                    &f.f.execution,
                    f.f.task.detail.summary.revision,
                    &NextPromptInvocation {
                        input: &f.input,
                        prepared: &f.prepared,
                        active_milliseconds: f.f.task.detail.usage.active_milliseconds,
                    },
                    |_| Ok(()),
                )
                .unwrap();
            f.refresh();
        } else {
            // Preserve genuine measured usage and attempt history, not defaults.
            f.acquire_inventory();
        }
        let before = f.f.task.detail.usage.clone();
        let ledger = f.f.ledger.clone();
        let candidate = candidate(&f.f);
        let (directory, _store, id, detail) = recover(f.f, candidate);
        assert_eq!(detail.summary.status, InvestigationStatus::Interrupted);
        assert_eq!(detail.usage, before);
        let reopened = JobStore::open(directory.path().join("state.db")).unwrap();
        // Public read boundaries dispatch through the same full family validator.
        assert_eq!(reopened.investigation(&id).unwrap(), detail);
        assert_eq!(
            reopened.investigation_ledger(&id).unwrap(),
            Some(ledger.clone())
        );
        assert!(reopened.investigation_result(&id).unwrap().is_none());
        assert!(reopened.investigation_pending_step(&id).unwrap().is_none());
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task(&tx, &id).unwrap();
        assert_eq!(task.phase, Phase::Terminal);
        assert!(task.pending.is_none());
        assert!(!task.approved);
        assert!(
            task.admission_state
                .as_ref()
                .unwrap()
                .current_call
                .is_none()
        );
        assert_eq!(load_ledger(&tx, &task).unwrap().as_ref(), Some(&ledger));
        assert_eq!(
            completion::next_prompt_invocations(&tx, &task)
                .unwrap()
                .len(),
            usize::from(!cloud)
        );
        assert_eq!(
            completion::attempts(&tx, &task).unwrap().len(),
            usize::from(!cloud)
        );
        assert_eq!(
            last_event(&tx, &id, detail.summary.last_event_sequence).kind,
            InvestigationEventKind::Interrupted
        );
    }
}

#[test]
fn next_prompt_recovery_invocation_keeps_call_charge_and_never_replays() {
    let mut f = Fixture::new(false);
    f.begin();
    let before = f.f.task.detail.usage.clone();
    let candidate = candidate(&f.f);
    let (directory, store, id, detail) = recover(f.f, candidate);
    assert_eq!(detail.summary.status, InvestigationStatus::OutcomeUnknown);
    assert_eq!(detail.usage.model_invocations, before.model_invocations);
    assert_eq!(
        detail.usage.generated_token_reservations,
        before.generated_token_reservations
    );
    assert_eq!(detail.usage.tool_actions, before.tool_actions);
    assert_eq!(detail.usage.reported_input_tokens, None);
    assert_eq!(detail.usage.reported_output_tokens, None);
    assert!(!detail.summary.has_result);
    let reopened = JobStore::open(directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task(&tx, &id).unwrap();
    assert_eq!(
        completion::next_prompt_invocations(&tx, &task)
            .unwrap()
            .len(),
        1
    );
    let responses: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM investigation_responses WHERE task_id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(responses, 0);
    assert_eq!(
        store.get(task.detail.summary.job_id).unwrap().status,
        "interrupted"
    );
}

#[test]
fn next_prompt_recovery_source_unknown_retains_read_accounting_and_validation_charge() {
    for charged in [false, true] {
        let f = fixture::source(charged);
        let before = f.task.detail.usage.clone();
        let ledger = f.ledger.clone();
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let pending = source_validation::validate_saved_pending(&tx, &f.task).unwrap();
        let step = pending.reservation.binding.step_id.clone();
        let projected = read_reservations::project_accounting(
            &pending.reservation,
            pending.charge.as_ref(),
            None,
            true,
        )
        .unwrap();
        assert_eq!(projected.validation_bytes, if charged { 12 } else { 0 });
        assert!(
            read_reservations::pending_storage_claims(
                &tx,
                Some(&f.task.detail.summary.investigation_id)
            )
            .unwrap()
                > 0
        );
        tx.commit().unwrap();
        let candidate = candidate(&f);
        let (directory, _store, id, detail) = recover(f, candidate);
        assert_eq!(detail.summary.status, InvestigationStatus::OutcomeUnknown);
        assert_eq!(detail.usage, before);
        let reopened = JobStore::open(directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task(&tx, &id).unwrap();
        assert!(task.pending_source_read.is_none());
        let row = read_reservations::load(&tx, &id, &step).unwrap().unwrap();
        let disposition = row.disposition.unwrap();
        assert_eq!(disposition.kind, DispositionKind::OutcomeUnknown);
        assert_eq!(disposition.accounting, projected);
        assert_eq!(disposition.accounting.reserved_unknown_tool_actions, 1);
        assert_eq!(disposition.accounting.reserved_unknown_evidence_requests, 1);
        assert_eq!(disposition.accounting.executed_tool_actions, 0);
        assert_eq!(
            read_reservations::pending_storage_claims(&tx, Some(&id)).unwrap(),
            0
        );
        assert_eq!(load_ledger(&tx, &task).unwrap().as_ref(), Some(&ledger));
        assert_eq!(
            completion::next_prompt_invocations(&tx, &task)
                .unwrap()
                .len(),
            2
        );
        assert!(completion::attempts(&tx, &task).unwrap().is_empty());
    }
}

#[test]
fn next_prompt_recovery_stale_candidate_and_foreign_lock_make_no_changes() {
    let mut f = Fixture::new(false);
    let old = candidate(&f.f);
    let id = f.id().to_owned();
    f.f.store.cancel_next_prompt_investigation(&id).unwrap();
    f.refresh();
    let before = saved(&f.f);
    let InitialFixture {
        directory,
        mut store,
        execution,
        task,
        ..
    } = f.f;
    drop(execution);
    let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
    let lock = locks.try_reserve(old.lock_target()).unwrap();
    assert!(store.recover_investigation(&old, lock).unwrap().is_none());
    let actual = store.investigation_recovery_candidates().unwrap().remove(0);
    let other = Fixture::new(false);
    let foreign = candidate(&other.f);
    let InitialFixture {
        directory: other_dir,
        store: other_store,
        execution: other_owner,
        ..
    } = other.f;
    drop(other_owner);
    let foreign_locks =
        JobExecutionLocks::open(other_dir.path(), other_store.execution_namespace()).unwrap();
    let lock = foreign_locks.try_reserve(foreign.lock_target()).unwrap();
    assert_eq!(
        store.recover_investigation(&actual, lock),
        Err(InvestigationStoreError::Ownership)
    );
    // Compare all immutable inputs/invocations/events and the mutable task bytes.
    let id = task.detail.summary.investigation_id;
    let mut after = vec![];
    for (table, column, key) in [
        ("investigation_tasks", "record", "id"),
        ("investigation_events", "body", "task_id"),
        ("investigation_invocations", "body", "task_id"),
        ("investigation_responses", "body", "task_id"),
        ("investigation_inputs", "body", "task_id"),
    ] {
        after.extend(
            store
                .conn
                .prepare(&format!(
                    "SELECT {column} FROM {table} WHERE {key}=?1 ORDER BY {column}"
                ))
                .unwrap()
                .query_map([&id], |r| r.get::<_, Vec<u8>>(0))
                .unwrap()
                .map(Result::unwrap),
        );
    }
    assert_eq!(after, before);
}

#[test]
fn next_prompt_recovery_corrupt_task_or_pending_journal_fails_closed() {
    for journal in [false, true] {
        let f = if journal {
            fixture::source(true)
        } else {
            Fixture::new(false).f
        };
        let candidate = candidate(&f);
        let InitialFixture {
            directory,
            mut store,
            execution,
            task,
            ..
        } = f;
        let id = task.detail.summary.investigation_id;
        if journal {
            store
                .conn
                .execute_batch("DROP TRIGGER investigation_read_reservations_no_update")
                .unwrap();
            store
                .conn
                .execute(
                    "UPDATE investigation_read_reservations SET body_hash=?2 WHERE task_id=?1",
                    params![id, "0".repeat(64)],
                )
                .unwrap();
            store.conn.execute_batch("CREATE TRIGGER investigation_read_reservations_no_update BEFORE UPDATE ON investigation_read_reservations BEGIN SELECT RAISE(ABORT, 'immutable investigation read record'); END").unwrap();
        } else {
            store
                .conn
                .execute(
                    "UPDATE investigation_tasks SET record_hash=?2 WHERE id=?1",
                    params![id, "0".repeat(64)],
                )
                .unwrap();
        }
        assert!(store.investigation_recovery_candidates().is_err());
        drop(execution);
        let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
        let lock = locks.try_reserve(candidate.lock_target()).unwrap();
        assert!(store.recover_investigation(&candidate, lock).is_err());
        let live: i64 = store
            .conn
            .query_row(
                "SELECT live FROM investigation_tasks WHERE id=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live, 1);
        if journal {
            let count: i64 = store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM investigation_read_dispositions WHERE task_id=?1",
                    [&id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 0);
        }
    }
}

fn last_event(tx: &Connection, id: &str, sequence: u64) -> InvestigationEvent {
    let (body, hash): (Vec<u8>, String) = tx
        .query_row(
            "SELECT body,body_hash FROM investigation_events WHERE task_id=?1 AND sequence=?2",
            params![id, i64::try_from(sequence).unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    decode(&body, &hash).unwrap()
}

#[path = "next_prompt_job_cancel_tests.rs"]
mod job_cancel_tests;
