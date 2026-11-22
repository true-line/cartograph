use super::*;
#[path = "source_validation_fixture.rs"]
pub mod fixture_data;
use fixture_data::*;

// AC-0287: witness, actual full-file validation usage, event and task CAS are
// one commit; eventual tool/evidence-request slots remain reserved, not executed.
#[test]
fn source_validation_charge_commits_exact_witness_usage_and_reopens() {
    let (mut f, r) = fixture();
    let before = f.task.detail.usage.clone();
    let hash = r.fingerprint().unwrap();
    let receipt = charge(&mut f, &r).unwrap();
    assert!(receipt.matches(
        &basis(&f, &hash),
        &r.source,
        &r.binding.base_ledger_hash,
        r.source.captured.file.byte_len
    ));
    assert_eq!(receipt.task_id(), f.basis.task_id);
    assert_eq!(receipt.step_id(), f.step.step_id);
    assert_eq!(receipt.reservation_hash(), hash);
    assert_eq!(
        receipt.detail().usage.captured_validation_bytes,
        before.captured_validation_bytes + r.source.captured.file.byte_len
    );
    assert_eq!(receipt.detail().usage.tool_actions, before.tool_actions);
    assert_eq!(
        receipt.detail().usage.evidence_requests,
        before.evidence_requests
    );
    assert_eq!(receipt.detail().citations, f.task.detail.citations);
    f.store
        .verify_source_validation_access(&f._execution_guard, &receipt)
        .unwrap();
    assert_eq!(
        saved(&f).charge,
        Some(ValidationCharge::for_reservation(&r).unwrap())
    );
    assert!(saved(&f).disposition.is_none());
    assert_eq!(
        &f.store.investigation(&f.basis.task_id).unwrap(),
        receipt.detail()
    );
    let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
    reopened
        .verify_source_validation_access(&f._execution_guard, &receipt)
        .unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &f.basis.task_id).unwrap();
    assert_eq!(task.detail, *receipt.detail());
    assert_eq!(task.phase, Phase::Reservation);
    #[cfg(unix)]
    {
        // AC-0327: actual reopened charged reservation remains live in replay.
        let retained = validate_saved_pending(&tx, &task).unwrap();
        let bytes = serde_json::to_vec(&retained.reservation).unwrap().len()
            + serde_json::to_vec(&retained.charge).unwrap().len();
        assert!(
            next_prompt_pending_source_workspace().unwrap()
                >= bytes + next_prompt_saved_validation_workspace().unwrap().0
        );
    }
}

// AC-0287: same descriptor/context is insufficient when task, invocation or
// reservation differ. An opaque receipt cannot be copied into a second read.
#[test]
fn source_validation_receipt_rejects_task_step_reservation_swaps() {
    let (mut f, r) = fixture();
    let receipt = charge(&mut f, &r).unwrap();
    let hash = r.fingerprint().unwrap();
    for field in ["task", "step", "reservation"] {
        let mut expected = basis(&f, &hash);
        match field {
            "task" => expected.task_id = "another-task",
            "step" => expected.step_id = "another-step",
            _ => {
                expected.reservation_hash =
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }
        }
        assert!(!receipt.matches(
            &expected,
            &r.source,
            &r.binding.base_ledger_hash,
            r.source.captured.file.byte_len
        ));
    }
}

// AC-0287: every expected identity and descriptor is a checked CAS input.
#[test]
fn source_validation_charge_rejects_swapped_basis_without_writes() {
    for field in [
        "task",
        "step",
        "reservation",
        "revision",
        "target",
        "ledger",
    ] {
        let (mut f, r) = fixture();
        let hash = r.fingerprint().unwrap();
        let mut task = f.basis.task_id.clone();
        let mut step = f.step.step_id.clone();
        let mut reservation = hash.clone();
        let mut revision = f.task.detail.summary.revision;
        let mut target = r.source.clone();
        let mut ledger = r.binding.base_ledger_hash.clone();
        match field {
            "task" => task = "another-task".into(),
            "step" => step = "another-step".into(),
            "reservation" => reservation = "a".repeat(64),
            "revision" => revision += 1,
            "target" => {
                target.source.commit_sha = "different".into();
                target.target_id = target.content_id().unwrap();
            }
            _ => ledger = "a".repeat(64),
        }
        let expected = SourceValidationBasis {
            task_id: &task,
            step_id: &step,
            reservation_hash: &reservation,
            revision,
        };
        assert!(
            f.store
                .charge_prepared_source_validation(&f._execution_guard, &expected, &target, &ledger)
                .is_err(),
            "{field}"
        );
        assert!(saved(&f).charge.is_none(), "{field}");
        let tx = f.store.conn.unchecked_transaction().unwrap();
        assert_eq!(
            load_next_prompt_task_base(&tx, &f.basis.task_id)
                .unwrap()
                .detail,
            f.task.detail,
            "{field}"
        );
    }
}

// AC-0287: a completed charge cannot mint another receipt/read authorization.
#[test]
fn source_validation_charge_is_once_only() {
    let (mut f, r) = fixture();
    let receipt = charge(&mut f, &r).unwrap();
    assert!(matches!(
        charge(&mut f, &r),
        Err(InvestigationStoreError::Stale)
    ));
    f.task.detail.summary.revision = receipt.detail.summary.revision;
    assert!(matches!(
        charge(&mut f, &r),
        Err(InvestigationStoreError::Conflict)
    ));
    assert_eq!(
        saved(&f).charge,
        Some(ValidationCharge::for_reservation(&r).unwrap())
    );
}

// AC-0287: cancellation/owner fences remain real before charge AND after its
// successful commit; validation work stays charged when source access is denied.
#[test]
fn source_validation_access_fence_preserves_charge_after_cancellation_or_owner_loss() {
    for task_cancel in [false, true] {
        let (mut f, r) = fixture();
        let receipt = charge(&mut f, &r).unwrap();
        if task_cancel {
            let tx = f.store.conn.transaction().unwrap();
            let mut task = load_next_prompt_task_base(&tx, &f.basis.task_id).unwrap();
            let revision = task.detail.summary.revision;
            task.detail.summary.cancel_requested = true;
            task.detail.summary.actions.can_cancel = false;
            bump(&tx, &mut task).unwrap();
            save_next_prompt_task(&tx, &task, revision, false).unwrap();
            tx.commit().unwrap();
        } else {
            f.store
                .conn
                .execute(
                    "UPDATE jobs SET status='cancelled' WHERE id=?1",
                    [f.task.detail.summary.job_id],
                )
                .unwrap();
        }
        assert!(matches!(
            f.store
                .verify_source_validation_access(&f._execution_guard, &receipt),
            Err(InvestigationStoreError::Cancelled | InvestigationStoreError::Ownership)
        ));
        assert_eq!(
            saved(&f).charge,
            Some(ValidationCharge::for_reservation(&r).unwrap())
        );
        let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
        assert!(
            reopened
                .verify_source_validation_access(&f._execution_guard, &receipt)
                .is_err()
        );
    }
}

// AC-0287: deterministic write/ack faults preserve atomicity. After-COMMIT ack
// loss yields no receipt, while fresh read sees actual witness/charge retained.
#[test]
fn source_validation_charge_faults_rollback_or_preserve_committed_unknown_ack() {
    for failure in [
        ChargeFailure::AfterWitness,
        ChargeFailure::AfterTaskSave,
        ChargeFailure::BeforeCommit,
        ChargeFailure::AfterCommit,
    ] {
        let (mut f, r) = fixture();
        let before = f.task.detail.clone();
        CHARGE_FAILURE.with(|slot| slot.set(Some(failure)));
        assert!(matches!(
            charge(&mut f, &r),
            Err(InvestigationStoreError::Storage)
        ));
        let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task_base(&tx, &f.basis.task_id).unwrap();
        let saved = read_reservations::load(&tx, &f.basis.task_id, &f.step.step_id)
            .unwrap()
            .unwrap();
        if failure == ChargeFailure::AfterCommit {
            assert!(saved.charge.is_some());
            assert_eq!(
                task.detail.usage.captured_validation_bytes,
                before.usage.captured_validation_bytes + r.source.captured.file.byte_len
            );
        } else {
            assert!(saved.charge.is_none());
            assert_eq!(task.detail, before);
        }
        assert!(saved.disposition.is_none());
    }
}

#[test]
fn source_validation_archive_identity_classifies_missing_and_corrupt_rows() {
    // AC-0287: retained identity defects must not look like uncertain storage I/O.
    let (f, reservation) = fixture();
    assert_eq!(
        retained_archive_hash(&f.store.conn, &f.basis.task_id).unwrap(),
        reservation.binding.archive_hash
    );
    assert!(matches!(
        retained_archive_hash(&f.store.conn, "absent-task"),
        Err(InvestigationStoreError::Missing)
    ));
    let tx = f.store.conn.unchecked_transaction().unwrap();
    tx.execute_batch("DROP TRIGGER investigation_coverage_archives_no_update; PRAGMA ignore_check_constraints=ON;").unwrap();
    for bad in ["short".to_owned(), "G".repeat(64)] {
        tx.execute(
            "UPDATE investigation_coverage_archives SET body_hash=?2 WHERE task_id=?1",
            params![f.basis.task_id, bad],
        )
        .unwrap();
        assert!(matches!(
            retained_archive_hash(&tx, &f.basis.task_id),
            Err(InvestigationStoreError::Invalid)
        ));
    }
    tx.rollback().unwrap();
    assert_eq!(
        retained_archive_hash(&f.store.conn, &f.basis.task_id).unwrap(),
        reservation.binding.archive_hash
    );
}

#[test]
fn source_validation_storage_projection_bounds_actual_committed_growth() {
    // AC-0287: allocation sizing and the real writer use one production projection.
    // Unspent claim bytes are excluded; the delta is the real write, including
    // task growth and the same logical row overhead as capacity accounting.
    let (mut f, reservation) = fixture();
    let (bound, task_before, global_before) = {
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let bound = validation_storage_bound(&tx, &f.task, &reservation).unwrap();
        let values = (
            bound,
            actual_stored_bytes(&tx, Some(&f.basis.task_id)).unwrap(),
            actual_stored_bytes(&tx, None).unwrap(),
        );
        tx.commit().unwrap();
        values
    };
    let receipt = charge(&mut f, &reservation).unwrap();
    let tx = f.store.conn.unchecked_transaction().unwrap();
    let task_after = actual_stored_bytes(&tx, Some(&f.basis.task_id)).unwrap();
    let global_after = actual_stored_bytes(&tx, None).unwrap();
    assert!(bound > 0);
    assert_eq!(task_after - task_before, bound);
    assert_eq!(global_after - global_before, bound);
    assert_eq!(
        receipt.detail().usage.captured_validation_bytes,
        reservation.binding.base_usage.captured_validation_bytes
            + reservation.source.captured.file.byte_len
    );
}

#[test]
fn source_validation_spends_own_claim_at_exact_task_capacity() {
    // AC-0287: seed a valid internal reservation whose remaining settlement
    // allocation fills the logical ceiling. This tests stage spending; the
    // production reservation-admission derivation is outside this fixture.
    let (mut f, mut reservation) = fixture();
    let maximum = TASK_CAPACITY - TERMINAL_RESERVE;
    let tx = f.store.conn.unchecked_transaction().unwrap();
    reservation.storage_claim.validation_bytes =
        validation_storage_bound(&tx, &f.task, &reservation).unwrap();
    tx.execute_batch("DROP TRIGGER investigation_read_reservations_no_update")
        .unwrap();
    for _ in 0..8 {
        f.task
            .pending_source_read
            .as_mut()
            .unwrap()
            .reservation_hash = reservation.fingerprint().unwrap();
        let body = encode(&reservation, MAX_MANIFEST_BYTES).unwrap();
        tx.execute(
            "UPDATE investigation_read_reservations SET body=?2,body_hash=?3 WHERE task_id=?1",
            params![f.basis.task_id, body, core_prov::content_hash(&body)],
        )
        .unwrap();
        let task_bytes = encode(&f.task, MAX_RECORD_BYTES).unwrap();
        tx.execute(
            "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
            params![
                f.basis.task_id,
                task_bytes,
                core_prov::content_hash(&task_bytes)
            ],
        )
        .unwrap();
        // Restore the exact namespace before each coherent capacity snapshot.
        tx.execute_batch("CREATE TRIGGER investigation_read_reservations_no_update BEFORE UPDATE ON investigation_read_reservations BEGIN SELECT RAISE(ABORT, 'immutable investigation read record'); END").unwrap();
        let occupied = stored_bytes(&tx, Some(&f.basis.task_id)).unwrap();
        if occupied == maximum {
            break;
        }
        if occupied < maximum {
            reservation.storage_claim.settlement_bytes += maximum - occupied;
        } else {
            reservation.storage_claim.settlement_bytes -= occupied - maximum;
        }
        tx.execute_batch("DROP TRIGGER investigation_read_reservations_no_update")
            .unwrap();
    }
    assert_eq!(stored_bytes(&tx, Some(&f.basis.task_id)).unwrap(), maximum);
    validate_saved_pending(&tx, &f.task).unwrap();
    tx.commit().unwrap();
    charge(&mut f, &reservation).unwrap();
    let tx = f.store.conn.unchecked_transaction().unwrap();
    assert_eq!(stored_bytes(&tx, Some(&f.basis.task_id)).unwrap(), maximum);
    assert_eq!(
        read_reservations::pending_storage_claims(&tx, Some(&f.basis.task_id)).unwrap(),
        reservation.storage_claim.settlement_bytes
    );
    assert_eq!(
        read_reservations::pending_event_claims(&tx, &f.basis.task_id).unwrap(),
        2
    );
}

#[test]
fn source_validation_one_byte_short_refuses_without_durable_changes() {
    // AC-0287: an authentic reservation with an insufficient stage allocation
    // cannot issue a receipt, charge usage, or consume settlement headroom.
    let (mut f, mut reservation) = fixture();
    let tx = f.store.conn.unchecked_transaction().unwrap();
    reservation.storage_claim.validation_bytes =
        validation_storage_bound(&tx, &f.task, &reservation).unwrap() - 1;
    f.task
        .pending_source_read
        .as_mut()
        .unwrap()
        .reservation_hash = reservation.fingerprint().unwrap();
    let body = encode(&reservation, MAX_MANIFEST_BYTES).unwrap();
    tx.execute_batch("DROP TRIGGER investigation_read_reservations_no_update")
        .unwrap();
    tx.execute(
        "UPDATE investigation_read_reservations SET body=?2,body_hash=?3 WHERE task_id=?1",
        params![f.basis.task_id, body, core_prov::content_hash(&body)],
    )
    .unwrap();
    tx.execute_batch("CREATE TRIGGER investigation_read_reservations_no_update BEFORE UPDATE ON investigation_read_reservations BEGIN SELECT RAISE(ABORT, 'immutable investigation read record'); END").unwrap();
    let task_body = encode(&f.task, MAX_RECORD_BYTES).unwrap();
    tx.execute(
        "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
        params![
            f.basis.task_id,
            task_body,
            core_prov::content_hash(&task_body)
        ],
    )
    .unwrap();
    validate_saved_pending(&tx, &f.task).unwrap();
    let before_bytes = stored_bytes(&tx, Some(&f.basis.task_id)).unwrap();
    let before_events: i64 = tx
        .query_row(
            "SELECT count(*) FROM investigation_events WHERE task_id=?1",
            [&f.basis.task_id],
            |row| row.get(0),
        )
        .unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        charge(&mut f, &reservation),
        Err(InvestigationStoreError::Capacity)
    ));
    assert!(saved(&f).charge.is_none());
    assert!(saved(&f).disposition.is_none());
    let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &f.basis.task_id).unwrap();
    assert_eq!(encode(&task, MAX_RECORD_BYTES).unwrap(), task_body);
    assert_eq!(
        stored_bytes(&tx, Some(&f.basis.task_id)).unwrap(),
        before_bytes
    );
    let after_events: i64 = tx
        .query_row(
            "SELECT count(*) FROM investigation_events WHERE task_id=?1",
            [&f.basis.task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after_events, before_events);
    assert_eq!(
        read_reservations::pending_event_claims(&tx, &f.basis.task_id).unwrap(),
        3
    );
}
