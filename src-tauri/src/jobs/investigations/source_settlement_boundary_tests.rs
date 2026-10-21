//! AC-0287: exact stage allocations and bounded cancellation reserve. These
//! tests seed private allocations before the immutable validation charge;
//! production source admission independently derives and verifies its claims.
use super::*;

#[test]
fn source_settlement_exact_stage_allocation_accepts_all_alternatives_and_one_less_rolls_back() {
    for (kind, reason, charged) in [
        (DispositionKind::Acquired, None, true),
        (
            DispositionKind::ReadUnavailable,
            Some(UnavailableReason::PinnedTargetAbsent),
            true,
        ),
        (
            DispositionKind::ReadUnavailable,
            Some(UnavailableReason::RangeOutOfBounds),
            true,
        ),
        (DispositionKind::ReleasedBeforeRead, None, false),
        (DispositionKind::ReleasedBeforeRead, None, true),
        (DispositionKind::KnownReadDiscarded, None, true),
        (DispositionKind::BoundInvariantDefect, None, true),
        (DispositionKind::OutcomeUnknown, None, false),
        (DispositionKind::OutcomeUnknown, None, true),
    ] {
        for short in [false, true] {
            let mut fixture = Fixture::new();
            let response = fixture.response();
            let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
            let projected_charge =
                source_validation::project_validation(&tx, &fixture.f.task, &fixture.r).unwrap();
            let before = if charged {
                &projected_charge.task
            } else {
                &fixture.f.task
            };
            let saved = SavedRead {
                reservation: fixture.r.clone(),
                charge: charged.then(|| projected_charge.charge.clone()),
                disposition: None,
            };
            let ledger = if kind == DispositionKind::Acquired {
                Some(
                    envelope_projection::sizing_acquired_ledger(
                        &fixture.f.ledger,
                        &fixture.r.source,
                    )
                    .unwrap(),
                )
            } else if kind == DispositionKind::ReadUnavailable {
                Some(fixture.f.ledger.clone())
            } else {
                None
            };
            // Private serializer projection only: placeholder source-text hashes
            // do not become an actual plan or persistable acquisition.
            let exact = materialize(
                &tx,
                before,
                &saved,
                &response,
                &fixture.f.step.step_id,
                ledger,
                kind,
                reason.clone(),
                before.detail.usage.active_milliseconds + 1,
            )
            .unwrap()
            .additional_bytes;
            drop(tx);
            fixture.r.storage_claim.settlement_bytes = exact - usize::from(short);
            replace_reservation(&mut fixture.f, &fixture.r);
            if charged {
                fixture.charge();
            }
            let active = fixture.f.task.detail.usage.active_milliseconds + 1;
            let candidate = matches!(
                kind,
                DispositionKind::Acquired | DispositionKind::ReadUnavailable
            )
            .then(|| fixture.candidate(reason.clone()));
            let outcome = if let Some((input, prepared)) = &candidate {
                let c = SourceContinuation {
                    current: &fixture.current,
                    current_prepared: &fixture.prepared,
                    candidate: input,
                    candidate_prepared: prepared,
                    proof: &fixture.proof,
                };
                if let Some(reason) = &reason {
                    SourceSettlement::Unavailable {
                        reason: reason.clone(),
                        continuation: c,
                    }
                } else {
                    SourceSettlement::Acquired(c)
                }
            } else {
                SourceSettlement::Terminal(kind)
            };
            let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
            let before_bytes = actual_stored_bytes(&tx, Some(&fixture.f.basis.task_id)).unwrap();
            let before_record = encode(&fixture.f.task, MAX_RECORD_BYTES).unwrap();
            let plan = project(&tx, &fixture.f.task, outcome, active).unwrap();
            assert_eq!(
                plan.additional_bytes(),
                exact,
                "{kind:?}, charged={charged}"
            );
            let result = persist(&tx, &fixture.f.task, plan);
            if short {
                assert!(matches!(result, Err(InvestigationStoreError::Capacity)));
                tx.rollback().unwrap();
                let reopened = JobStore::open(fixture.f.directory.path().join("state.db")).unwrap();
                let tx = reopened.conn.unchecked_transaction().unwrap();
                let task = load_next_prompt_task_base(&tx, &fixture.f.basis.task_id).unwrap();
                assert_eq!(encode(&task, MAX_RECORD_BYTES).unwrap(), before_record);
                assert_eq!(
                    actual_stored_bytes(&tx, Some(&fixture.f.basis.task_id)).unwrap(),
                    before_bytes
                );
                let saved =
                    read_reservations::load(&tx, &fixture.f.basis.task_id, &fixture.f.step.step_id)
                        .unwrap()
                        .unwrap();
                assert!(saved.disposition.is_none());
                assert_eq!(saved.charge.is_some(), charged);
                assert_eq!(
                    read_reservations::pending_storage_claims(&tx, Some(&fixture.f.basis.task_id))
                        .unwrap(),
                    fixture.r.storage_claim.bytes(charged).unwrap()
                );
            } else {
                result.unwrap();
                tx.commit().unwrap();
            }
        }
    }
}

#[test]
fn source_settlement_failed_validation_allocation_can_release_without_inventing_charge_or_read() {
    let mut fixture = Fixture::new();
    fixture.r.storage_claim.validation_bytes -= 1;
    replace_reservation(&mut fixture.f, &fixture.r);
    let result = fixture.f.store.charge_prepared_source_validation(
        &fixture.f._execution_guard,
        &source_validation::SourceValidationBasis {
            task_id: &fixture.f.basis.task_id,
            step_id: &fixture.f.step.step_id,
            reservation_hash: &fixture.r.fingerprint().unwrap(),
            revision: fixture.f.task.detail.summary.revision,
        },
        &fixture.r.source,
        &fixture.r.binding.base_ledger_hash,
    );
    assert!(matches!(result, Err(InvestigationStoreError::Capacity)));
    fixture.refresh();
    let detail = fixture
        .f
        .store
        .settle_next_prompt_source(
            &fixture.f._execution_guard,
            fixture.f.task.detail.summary.revision,
            SourceSettlement::Terminal(DispositionKind::ReleasedBeforeRead),
            fixture.f.task.detail.usage.active_milliseconds,
            |_| panic!("release must not claim retained source validity"),
        )
        .unwrap();
    assert_eq!(detail.usage, fixture.r.binding.base_usage);
    let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
    let saved = read_reservations::load(&tx, &fixture.f.basis.task_id, &fixture.f.step.step_id)
        .unwrap()
        .unwrap();
    assert!(saved.charge.is_none());
    let d = saved.disposition.unwrap();
    assert_eq!(d.kind, DispositionKind::ReleasedBeforeRead);
    assert_eq!(d.accounting, read_reservations::ReadAccounting::default());
    assert!(
        completion::attempts(
            &tx,
            &load_next_prompt_task_base(&tx, &fixture.f.basis.task_id).unwrap()
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn source_settlement_full_task_capacity_allows_actual_cancel_then_cancelled_release() {
    // This executes the valid per-task boundary, not a literal 256MiB fill.
    // The checked global algebra below pins the implicit terminal headroom
    // guaranteed by the unchanged maximum task count and per-task capacity.
    assert_eq!(MAX_TASKS.checked_mul(TASK_CAPACITY), Some(STORE_CAPACITY));
    assert_eq!(
        (MAX_TASKS - 1) * TASK_CAPACITY + (TASK_CAPACITY - TERMINAL_RESERVE) + TERMINAL_RESERVE,
        STORE_CAPACITY
    );
    for charged in [false, true] {
        let mut fixture = Fixture::new();
        let maximum = TASK_CAPACITY - TERMINAL_RESERVE;
        // Converge private seed bytes within ONE transaction. Changing the
        // decimal claim width can temporarily exceed the target by a few bytes;
        // no malformed intermediate task is committed or passed to a reader.
        let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
        for _ in 0..8 {
            let occupied = stored_bytes(&tx, Some(&fixture.f.basis.task_id)).unwrap();
            if occupied == maximum {
                break;
            }
            if occupied < maximum {
                fixture.r.storage_claim.settlement_bytes += maximum - occupied;
            } else {
                fixture.r.storage_claim.settlement_bytes -= occupied - maximum;
            }
            tx.execute_batch("DROP TRIGGER investigation_read_reservations_no_update")
                .unwrap();
            let body = encode(&fixture.r, MAX_MANIFEST_BYTES).unwrap();
            tx.execute(
                "UPDATE investigation_read_reservations SET body=?2,body_hash=?3 WHERE task_id=?1",
                params![
                    fixture.f.basis.task_id,
                    body,
                    core_prov::content_hash(&body)
                ],
            )
            .unwrap();
            tx.execute_batch("CREATE TRIGGER investigation_read_reservations_no_update BEFORE UPDATE ON investigation_read_reservations BEGIN SELECT RAISE(ABORT, 'immutable investigation read record'); END").unwrap();
            fixture
                .f
                .task
                .pending_source_read
                .as_mut()
                .unwrap()
                .reservation_hash = fixture.r.fingerprint().unwrap();
            let bytes = encode(&fixture.f.task, MAX_RECORD_BYTES).unwrap();
            tx.execute(
                "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
                params![
                    fixture.f.basis.task_id,
                    bytes,
                    core_prov::content_hash(&bytes)
                ],
            )
            .unwrap();
        }
        assert_eq!(
            stored_bytes(&tx, Some(&fixture.f.basis.task_id)).unwrap(),
            maximum
        );
        source_validation::validate_saved_pending(&tx, &fixture.f.task).unwrap();
        tx.commit().unwrap();
        if charged {
            fixture.charge();
        }
        {
            let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
            assert_eq!(
                stored_bytes(&tx, Some(&fixture.f.basis.task_id)).unwrap(),
                maximum
            );
        }
        fixture
            .f
            .store
            .cancel_next_prompt_investigation(&fixture.f.basis.task_id)
            .unwrap();
        fixture.refresh();
        {
            let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
            let occupied = stored_bytes(&tx, Some(&fixture.f.basis.task_id)).unwrap();
            assert!(occupied > maximum && occupied <= TASK_CAPACITY);
        }
        let detail = fixture
            .f
            .store
            .settle_next_prompt_source(
                &fixture.f._execution_guard,
                fixture.f.task.detail.summary.revision,
                SourceSettlement::Terminal(DispositionKind::ReleasedBeforeRead),
                fixture.f.task.detail.usage.active_milliseconds,
                |_| panic!("terminal release requires no source access"),
            )
            .unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Cancelled);
        assert!(detail.summary.cancel_requested);
        assert_eq!(
            detail.usage.captured_validation_bytes,
            if charged { 12 } else { 0 }
        );
        assert_eq!(
            detail.usage.tool_actions,
            fixture.r.binding.base_usage.tool_actions
        );
        let reopened = JobStore::open(fixture.f.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task_base(&tx, &fixture.f.basis.task_id).unwrap();
        assert_eq!(task.phase, Phase::Terminal);
        assert_eq!(
            read_reservations::pending_storage_claims(&tx, Some(&fixture.f.basis.task_id)).unwrap(),
            0
        );
    }
}
