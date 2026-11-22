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
                        &INVESTIGATION_V2_POLICY,
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
                ledger.clone(),
                kind,
                reason.clone(),
                before.detail.usage.active_milliseconds + 1,
                None,
            )
            .unwrap();
            let legacy = completion::with_observation_prefix_reread(|| {
                materialize(
                    &tx,
                    before,
                    &saved,
                    &response,
                    &fixture.f.step.step_id,
                    ledger,
                    kind,
                    reason.clone(),
                    before.detail.usage.active_milliseconds + 1,
                    None,
                )
            })
            .unwrap();
            assert_eq!(projection_contents(&exact), projection_contents(&legacy));
            let exact = exact.additional_bytes;
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
                    size_binding: None,
                    current: &fixture.current,
                    current_prepared: &fixture.prepared,
                    candidate: input,
                    candidate_prepared: prepared,
                    proof: &fixture.proof,
                    refusal: None,
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
            #[cfg(unix)]
            {
                // AC-0327: actual acquired/unavailable/terminal projections,
                // including the pre-refusal one-byte-short case. Payload is
                // only a lower bound on ownership, not measured peak or RSS.
                let p = &plan.projection;
                let wire = serde_json::to_vec(&(
                    &p.task,
                    &p.disposition,
                    &p.events,
                    &p.ledger,
                    &p.attempts,
                ))
                .unwrap()
                .len();
                let (retained, peak) = writer_workspace::workspace(0, 0, 0).unwrap();
                assert!(retained >= wire + std::mem::size_of_val(&plan));
                assert!(peak > retained);
            }
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

/// Compare every projected DTO/count/byte role; fresh UTC timestamp contents can
/// differ between independent reads, so retain their validated fixed widths.
fn projection_contents(projection: &SettlementProjection) -> serde_json::Value {
    let mut value = serde_json::json!({
        "task": &projection.task, "disposition": &projection.disposition,
        "events": &projection.events, "ledger": &projection.ledger,
        "attempts": &projection.attempts, "attempt_offset": projection.attempt_offset,
        "additional_bytes": projection.additional_bytes,
        "counts": [projection.counts.events, projection.counts.attempts as u64,
            projection.counts.input_rows as u64, projection.counts.reference_rows as u64,
            projection.counts.disposition_rows as u64],
    });
    let updated = &mut value["task"]["detail"]["updated_at"];
    assert_eq!(updated.as_str().unwrap().len(), 20);
    *updated = "<fresh UTC timestamp>".into();
    for event in value["events"].as_array_mut().unwrap() {
        assert_eq!(event["created_at"].as_str().unwrap().len(), 20);
        event["created_at"] = "<fresh UTC timestamp>".into();
    }
    value
}

#[test]
fn source_settlement_cancelled_terminal_projections_match_reread_control() {
    // AC-0287/#1581: the remaining six envelope alternatives, without repeating
    // the writer/one-less matrix. Acquired/unavailable cancellation is excluded.
    for (kind, charged) in [
        (DispositionKind::ReleasedBeforeRead, false),
        (DispositionKind::ReleasedBeforeRead, true),
        (DispositionKind::OutcomeUnknown, false),
        (DispositionKind::OutcomeUnknown, true),
        (DispositionKind::KnownReadDiscarded, true),
        (DispositionKind::BoundInvariantDefect, true),
    ] {
        let fixture = Fixture::new();
        let response = fixture.response();
        let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
        let charge =
            source_validation::project_validation(&tx, &fixture.f.task, &fixture.r).unwrap();
        let mut before = if charged {
            charge.task
        } else {
            fixture.f.task.clone()
        };
        before.detail.summary.cancel_requested = true;
        before.detail.summary.actions.can_cancel = false;
        let saved = SavedRead {
            reservation: fixture.r.clone(),
            charge: charged.then_some(charge.charge),
            disposition: None,
        };
        let project = || {
            materialize(
                &tx,
                &before,
                &saved,
                &response,
                &fixture.f.step.step_id,
                None,
                kind,
                None,
                before.detail.usage.active_milliseconds + 1,
                None,
            )
        };
        let actual = project().unwrap();
        let legacy = completion::with_observation_prefix_reread(project).unwrap();
        assert_eq!(projection_contents(&actual), projection_contents(&legacy));
    }
}

// AC-0582: authenticated policy and optional text-free diagnostic regression.
#[test]
fn source_sizing_uses_combined_hosted_history_policy_without_widening_legacy() {
    let fixture = Fixture::new();
    let combined = INVESTIGATION_V2_POLICY
        .with_hosted_generation()
        .unwrap()
        .with_parent_history_32768()
        .unwrap();
    assert_eq!(
        InvestigationPolicy::for_effective_limits(&combined.limits())
            .unwrap()
            .limits(),
        combined.limits()
    );
    let mut ledger = fixture.f.ledger.clone();
    ledger.supplied_history_hash = Some("a".repeat(64));
    for bytes in [16384, 16385, 23240, 32768, 32769] {
        ledger.supplied_history_bytes = bytes;
        assert_eq!(
            envelope_projection::sizing_acquired_ledger(
                &ledger,
                &fixture.r.source,
                &INVESTIGATION_V2_POLICY
            )
            .is_ok(),
            bytes <= 16384
        );
        assert_eq!(
            envelope_projection::sizing_acquired_ledger(&ledger, &fixture.r.source, &combined)
                .is_ok(),
            bytes <= 32768
        );
    }
}
