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
