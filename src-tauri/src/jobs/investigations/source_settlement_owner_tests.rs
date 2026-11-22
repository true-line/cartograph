//! AC-0287: actual execution/OS-lock authority. Native source outcomes and
//! retained-check success are scripted here; actual source/guard I/O is tested
//! by the app adapter, not claimed by these transaction tests.
use super::*;

#[test]
fn source_settlement_owner_checks_lease_revision_and_retained_gate_before_publication() {
    let mut fixture = Fixture::new();
    fixture.charge();
    let foreign = Fixture::new();
    let (candidate, prepared) = fixture.candidate(None);
    let revision = fixture.f.task.detail.summary.revision;
    let active = fixture.f.task.detail.usage.active_milliseconds + 1;
    let before = encode(&fixture.f.task, MAX_RECORD_BYTES).unwrap();
    for case in 0..3 {
        let c = SourceContinuation {
            size_binding: None,
            current: &fixture.current,
            current_prepared: &fixture.prepared,
            candidate: &candidate,
            candidate_prepared: &prepared,
            proof: &fixture.proof,
            refusal: None,
        };
        let execution = if case == 0 {
            &foreign.f._execution_guard
        } else {
            &fixture.f._execution_guard
        };
        let result = fixture.f.store.settle_next_prompt_source(
            execution,
            revision + u64::from(case == 1),
            SourceSettlement::Acquired(c),
            active,
            |_| Err(InvestigationStoreError::Conflict),
        );
        assert_eq!(
            result.unwrap_err(),
            match case {
                0 => InvestigationStoreError::Ownership,
                1 => InvestigationStoreError::Stale,
                _ => InvestigationStoreError::Conflict,
            }
        );
        fixture.refresh();
        assert_eq!(encode(&fixture.f.task, MAX_RECORD_BYTES).unwrap(), before);
        let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
        assert!(
            read_reservations::load(&tx, &fixture.f.basis.task_id, &fixture.f.step.step_id)
                .unwrap()
                .unwrap()
                .disposition
                .is_none()
        );
    }
    let c = SourceContinuation {
        size_binding: None,
        current: &fixture.current,
        current_prepared: &fixture.prepared,
        candidate: &candidate,
        candidate_prepared: &prepared,
        proof: &fixture.proof,
        refusal: None,
    };
    let detail = fixture
        .f
        .store
        .settle_next_prompt_source(
            &fixture.f._execution_guard,
            revision,
            SourceSettlement::Acquired(c),
            active,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(detail.usage.evidence_items, 1);
    assert_eq!(detail.usage.captured_validation_bytes, 12);
    fixture.refresh();
    assert_eq!(fixture.f.task.phase, Phase::Ready);
}

#[test]
fn source_settlement_orphan_requires_recovered_os_lock_and_preserves_unknown_slots() {
    use crate::job_execution::JobExecutionLocks;
    for charged in [false, true] {
        let mut fixture = Fixture::new();
        if charged {
            fixture.charge();
        }
        let f = fixture.f;
        let namespace = f.store.execution_namespace();
        let locks = JobExecutionLocks::open(f.directory.path(), namespace.clone()).unwrap();
        let candidate = InvestigationRecoveryCandidate {
            id: f.basis.task_id.clone(),
            revision: f.task.detail.summary.revision,
            identity: f.task.execution.clone(),
            target: JobLockTarget {
                namespace,
                id: f.task.detail.summary.job_id,
                existing: true,
            },
        };
        assert!(locks.try_reserve(candidate.lock_target()).is_err());
        drop(f._execution_guard);
        let reservation = locks.try_reserve(candidate.lock_target()).unwrap();
        let mut store = f.store;
        let expected = {
            let tx = store.conn.unchecked_transaction().unwrap();
            let saved = read_reservations::load(&tx, &f.basis.task_id, &f.step.step_id)
                .unwrap()
                .unwrap();
            read_reservations::project_accounting(
                &saved.reservation,
                saved.charge.as_ref(),
                None,
                true,
            )
            .unwrap()
        };
        let detail = store
            .settle_orphaned_next_prompt_source(&candidate, reservation)
            .unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::OutcomeUnknown);
        assert_eq!(
            detail.usage.tool_actions,
            fixture.r.binding.base_usage.tool_actions
        );
        assert_eq!(
            detail.usage.evidence_requests,
            fixture.r.binding.base_usage.evidence_requests
        );
        let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task_base(&tx, &f.basis.task_id).unwrap();
        let saved = read_reservations::load(&tx, &f.basis.task_id, &f.step.step_id)
            .unwrap()
            .unwrap();
        assert_eq!(saved.disposition.unwrap().accounting, expected);
        assert_eq!(
            task.detail.usage.captured_validation_bytes,
            if charged { 12 } else { 0 }
        );
        assert!(completion::attempts(&tx, &task).unwrap().is_empty());
        assert!(task.pending_source_read.is_none());
        assert_eq!(
            read_reservations::pending_storage_claims(&tx, Some(&f.basis.task_id)).unwrap(),
            0
        );
        assert_eq!(
            tx.query_row::<i64, _, _>(
                "SELECT live FROM investigation_tasks WHERE id=?1",
                [&f.basis.task_id],
                |r| r.get(0)
            )
            .unwrap(),
            0
        );
    }
}

#[test]
fn source_settlement_post_read_deadline_failure_discards_truthfully_without_acquisition() {
    let mut fixture = Fixture::new();
    fixture.charge();
    let (candidate, prepared) = fixture.candidate(None);
    let deadline = fixture.f.task.detail.limits.active_seconds * 1000;
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
                fixture.f.task.detail.summary.revision,
                SourceSettlement::Acquired(c),
                deadline,
                |_| panic!("exhausted deadline must fail before retention gate")
            )
            .unwrap_err(),
        InvestigationStoreError::Capacity
    );
    fixture.refresh();
    assert_eq!(fixture.f.task.phase, Phase::Reservation);
    let detail = fixture
        .f
        .store
        .settle_next_prompt_source(
            &fixture.f._execution_guard,
            fixture.f.task.detail.summary.revision,
            SourceSettlement::Terminal(DispositionKind::KnownReadDiscarded),
            deadline,
            |_| panic!("discard must not claim retained source validity"),
        )
        .unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(detail.usage.active_milliseconds, deadline);
    assert_eq!(detail.usage.captured_validation_bytes, 12);
    assert_eq!(detail.usage.evidence_items, 0);
    assert_eq!(
        detail.usage.tool_actions,
        fixture.r.binding.base_usage.tool_actions + 1
    );
    let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &fixture.f.basis.task_id).unwrap();
    assert!(completion::attempts(&tx, &task).unwrap().is_empty());
    assert_eq!(task.ledger_revision, Some(fixture.f.ledger.revision));
    let saved = read_reservations::load(&tx, &fixture.f.basis.task_id, &fixture.f.step.step_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        saved.disposition.unwrap().kind,
        DispositionKind::KnownReadDiscarded
    );
}

#[test]
fn source_settlement_generic_terminal_stop_cannot_bypass_pending_disposition() {
    for charged in [false, true] {
        let mut fixture = Fixture::new();
        if charged {
            fixture.charge();
        }
        let before = encode(&fixture.f.task, MAX_RECORD_BYTES).unwrap();
        assert_eq!(
            fixture
                .f
                .store
                .stop_next_prompt_investigation(
                    &fixture.f._execution_guard,
                    fixture.f.task.detail.summary.revision,
                    InvestigationStopReason::Operational,
                    fixture.f.task.detail.usage.active_milliseconds + 1
                )
                .unwrap_err(),
            InvestigationStoreError::Invalid
        );
        fixture.refresh();
        assert_eq!(encode(&fixture.f.task, MAX_RECORD_BYTES).unwrap(), before);
        let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
        let saved = read_reservations::load(&tx, &fixture.f.basis.task_id, &fixture.f.step.step_id)
            .unwrap()
            .unwrap();
        assert!(saved.disposition.is_none());
        assert_eq!(saved.charge.is_some(), charged);
        assert_eq!(
            read_reservations::pending_storage_claims(&tx, Some(&fixture.f.basis.task_id)).unwrap(),
            fixture.r.storage_claim.bytes(charged).unwrap()
        );
    }
}
