//! AC-0287: production projections and authenticated durable settlement, no
//! provider or source I/O. Read outcome classification is explicit fixture data.
use super::*;
#[path = "source_settlement_fixture.rs"]
mod fixture;
use fixture::*;

#[test]
fn source_settlement_terminal_matrix_preserves_exact_accounting_and_reopens() {
    for charged in [false, true] {
        for kind in [
            DispositionKind::ReleasedBeforeRead,
            DispositionKind::KnownReadDiscarded,
            DispositionKind::BoundInvariantDefect,
            DispositionKind::OutcomeUnknown,
        ] {
            if !charged
                && matches!(
                    kind,
                    DispositionKind::KnownReadDiscarded | DispositionKind::BoundInvariantDefect
                )
            {
                continue;
            }
            let mut fixture = Fixture::new();
            if charged {
                fixture.charge();
            }
            let f = &mut fixture.f;
            let tx = f.store.conn.unchecked_transaction().unwrap();
            let before = actual_stored_bytes(&tx, Some(&f.basis.task_id)).unwrap();
            let plan = project(
                &tx,
                &f.task,
                SourceSettlement::Terminal(kind),
                f.task.detail.usage.active_milliseconds,
            )
            .unwrap();
            assert!(plan.additional_bytes() <= fixture.r.storage_claim.settlement_bytes);
            assert_eq!(plan.counts().attempts, 0);
            assert_eq!(plan.counts().input_rows, 0);
            let next = persist(&tx, &f.task, plan).unwrap();
            assert!(
                actual_stored_bytes(&tx, Some(&f.basis.task_id))
                    .unwrap()
                    .saturating_sub(before)
                    <= fixture.r.storage_claim.settlement_bytes
            );
            assert_eq!(
                read_reservations::pending_storage_claims(&tx, Some(&f.basis.task_id)).unwrap(),
                0
            );
            let saved = read_reservations::load(&tx, &f.basis.task_id, &f.step.step_id)
                .unwrap()
                .unwrap();
            let accounting = saved.disposition.unwrap().accounting;
            assert_eq!(accounting.validation_bytes, if charged { 12 } else { 0 });
            assert_eq!(
                accounting.reserved_unknown_tool_actions,
                u32::from(kind == DispositionKind::OutcomeUnknown)
            );
            assert_eq!(
                accounting.executed_tool_actions,
                u32::from(matches!(
                    kind,
                    DispositionKind::KnownReadDiscarded | DispositionKind::BoundInvariantDefect
                ))
            );
            assert_eq!(
                next.detail.usage.captured_validation_bytes,
                fixture.r.binding.base_usage.captured_validation_bytes
                    + if charged { 12 } else { 0 }
            );
            tx.commit().unwrap();
            let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
            let tx = reopened.conn.unchecked_transaction().unwrap();
            let saved = load_next_prompt_task_base(&tx, &f.basis.task_id).unwrap();
            assert_eq!(saved.phase, Phase::Terminal);
            assert!(saved.pending_source_read.is_none());
            assert_eq!(saved.detail.citations, f.ledger.citations);
            assert!(completion::attempts(&tx, &saved).unwrap().is_empty());
        }
    }
}

#[test]
fn source_settlement_actual_acquired_and_unavailable_use_same_bounded_rows_as_projection() {
    for reason in [
        None,
        Some(UnavailableReason::PinnedTargetAbsent),
        Some(UnavailableReason::RangeOutOfBounds),
    ] {
        let mut fixture = Fixture::new();
        fixture.charge();
        let (candidate, prepared) = fixture.candidate(reason.clone());
        let continuation = SourceContinuation {
            size_binding: None,
            current: &fixture.current,
            current_prepared: &fixture.prepared,
            candidate: &candidate,
            candidate_prepared: &prepared,
            proof: &fixture.proof,
        };
        let outcome = match reason.clone() {
            None => SourceSettlement::Acquired(continuation),
            Some(reason) => SourceSettlement::Unavailable {
                reason,
                continuation,
            },
        };
        let before = fixture.f.task.detail.usage.clone();
        let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
        let plan = project(
            &tx,
            &fixture.f.task,
            outcome,
            before.active_milliseconds + 1,
        )
        .unwrap();
        assert!(plan.additional_bytes() <= fixture.r.storage_claim.settlement_bytes);
        assert_eq!(plan.counts().attempts, 1);
        assert_eq!(plan.counts().reference_rows, 0);
        let next = persist(&tx, &fixture.f.task, plan).unwrap();
        assert_eq!(next.detail.usage.tool_actions, before.tool_actions + 1);
        assert_eq!(
            next.detail.usage.evidence_requests,
            before.evidence_requests + 1
        );
        assert_eq!(
            next.detail.usage.evidence_items,
            before.evidence_items + usize::from(reason.is_none())
        );
        let entries = completion::attempts(&tx, &next).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(next.phase, Phase::Ready);
        assert!(next.pending_source_read.is_none());
        tx.commit().unwrap();
        let reopened = JobStore::open(fixture.f.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let next = load_next_prompt_task_base(&tx, &fixture.f.basis.task_id).unwrap();
        assert_eq!(completion::attempts(&tx, &next).unwrap().len(), 1);
        assert_eq!(
            next.ledger_revision,
            Some(fixture.f.ledger.revision + u64::from(reason.is_none()))
        );
        // Source bytes never enter a durable metadata row.
        let bodies: Vec<Vec<u8>> = tx
            .prepare("SELECT body FROM investigation_inputs WHERE task_id=?1")
            .unwrap()
            .query_map([&fixture.f.basis.task_id], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(bodies.iter().all(|body| {
            !body
                .windows(b"private text".len())
                .any(|b| b == b"private text")
        }));
    }
}

#[path = "source_settlement_owner_tests.rs"]
mod owner_tests;

#[path = "read_refusals_tests.rs"]
mod read_refusal_tests;

#[path = "source_settlement_boundary_tests.rs"]
mod boundary_tests;

#[path = "source_settlement_fault_tests.rs"]
mod fault_tests;

#[path = "source_continuation_snapshot_tests.rs"]
mod source_snapshot_tests;

// AC-0487: a different saved family is not authority for this current call.
#[test]
fn source_continuation_rejects_substituted_task_family() {
    let mut fixture = Fixture::new();
    fixture.charge();
    let (candidate, prepared) = fixture.candidate(None);
    let c = SourceContinuation {
        size_binding: None,
        current: &fixture.current,
        current_prepared: &fixture.prepared,
        candidate: &candidate,
        candidate_prepared: &prepared,
        proof: &fixture.proof,
    };
    let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
    let saved = source_validation::validate_saved_pending(&tx, &fixture.f.task).unwrap();
    let core =
        admission_contract::load_saved_call_basis_core(&tx, &fixture.f.task, &fixture.f.basis)
            .unwrap();
    let response = read_body(
        &tx,
        "investigation_responses",
        "step_id",
        &fixture.f.basis.task_id,
        &fixture.f.step.step_id,
    )
    .unwrap()
    .unwrap();
    let active = fixture.f.task.detail.usage.active_milliseconds + 1;
    assert!(
        validate_continuation(
            &tx,
            &fixture.f.task,
            &saved,
            &core.ledger,
            &core.metadata,
            &core.step,
            &response,
            &c,
            DispositionKind::Acquired,
            None,
            active
        )
        .is_ok()
    );
    let mut gated = fixture.f.task.clone();
    gated.detail.summary.specialist_id = SpecialistId::DomainAnalystV18;
    gated.detail.specialist = SpecialistId::DomainAnalystV18.definition().unwrap();
    let mut intent = serde_json::to_value(gated.completion_intent.as_ref().unwrap()).unwrap();
    intent["specialist"] = "domain-analyst@18".into();
    gated.completion_intent = Some(serde_json::from_value(intent).unwrap());
    assert!(
        validate_continuation(
            &tx,
            &gated,
            &saved,
            &core.ledger,
            &core.metadata,
            &core.step,
            &response,
            &c,
            DispositionKind::Acquired,
            None,
            active
        )
        .is_err()
    );
}

#[test]
fn source_settlement_nonempty_prefix_serialized_observation_matches_and_corruption_refuses() {
    // AC-0287/#1581: authentic acquired inventory prefix, not an empty first
    // source admission. The production constructor owns its only loaded prefix.
    let mut fixture = completion::FinishFixture::new(true);
    fixture.begin();
    let before = &fixture.base.f.task;
    let tx = fixture.base.f.store.conn.unchecked_transaction().unwrap();
    assert_eq!(completion::attempts(&tx, before).unwrap().len(), 1);
    let mut terminal = before.clone();
    terminal.phase = Phase::Terminal;
    let mut expected = terminal.clone();
    completion::project_observation(&tx, before, &mut expected, &[]).unwrap();
    assert!(
        !expected
            .detail
            .completion
            .as_ref()
            .unwrap()
            .assessment
            .is_empty()
    );
    let changes = tx.total_changes();
    for reread in [false, true] {
        let mut next = terminal.clone();
        completion::take_attempt_prefix_reads();
        let mut build = || {
            completion::project_observation_with_offset(&tx, before, &mut next, |_, offset| {
                assert_eq!(offset, 1);
                Ok(vec![])
            })
        };
        let (offset, suffix) = if reread {
            completion::with_observation_prefix_reread(build)
        } else {
            build()
        }
        .unwrap();
        assert_eq!(offset, 1);
        assert!(suffix.is_empty());
        assert_eq!(
            completion::take_attempt_prefix_reads(),
            if reread { 2 } else { 1 }
        );
        assert_eq!(
            encode(&next, MAX_RECORD_BYTES).unwrap(),
            encode(&expected, MAX_RECORD_BYTES).unwrap()
        );
        assert_eq!(tx.total_changes(), changes);
    }
    let id = &before.detail.summary.investigation_id;
    let mut row = completion::attempts(&tx, before).unwrap().remove(0);
    row.record.input_ledger_hash = "0".repeat(64);
    row.record.attempt_id = row.record.content_id().unwrap();
    completion::rewrite_test_body(
        &tx,
        "completion_attempts",
        id,
        "sequence",
        &1i64,
        &encode(&row, MAX_RECORD_BYTES).unwrap(),
    );
    for reread in [false, true] {
        let called = std::cell::Cell::new(false);
        let mut next = terminal.clone();
        let mut build = || {
            completion::project_observation_with_offset(&tx, before, &mut next, |_, _| {
                called.set(true);
                Ok(vec![])
            })
        };
        let result = if reread {
            completion::with_observation_prefix_reread(build)
        } else {
            build()
        };
        assert!(result.is_err());
        assert!(!called.get());
        assert_eq!(
            encode(&next, MAX_RECORD_BYTES).unwrap(),
            encode(&terminal, MAX_RECORD_BYTES).unwrap()
        );
    }
    drop(tx); // Corruption is rolled back; the old standalone loader stays usable.
    let mut standalone = terminal.clone();
    completion::project_observation(&fixture.base.f.store.conn, before, &mut standalone, &[])
        .unwrap();
    assert_eq!(
        encode(&standalone, MAX_RECORD_BYTES).unwrap(),
        encode(&expected, MAX_RECORD_BYTES).unwrap()
    );
    assert!(
        completion::project_observation_with_offset(
            &fixture.base.f.store.conn,
            before,
            &mut standalone,
            |_, _| panic!("autocommit constructor must reject before suffix construction")
        )
        .is_err()
    );
}
