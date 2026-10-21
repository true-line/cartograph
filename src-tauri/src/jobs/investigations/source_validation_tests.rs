use super::*;
#[path = "source_validation_fixture.rs"]
pub(crate) mod fixture_data;
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
    assert!(matches!(
        f.store.investigation(&f.basis.task_id),
        Err(InvestigationStoreError::Invalid)
    ));
    let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
    reopened
        .verify_source_validation_access(&f._execution_guard, &receipt)
        .unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &f.basis.task_id).unwrap();
    assert_eq!(task.detail, *receipt.detail());
    assert_eq!(task.phase, Phase::Reservation);
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
