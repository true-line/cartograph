//! AC-0287: readonly source continuation projection shares actual settlement
//! DTOs. These native outcomes are scripted; no source or provider is accessed.
use super::*;
use crate::jobs::investigations::source_settlement::source_snapshot::*;

fn snapshot(
    fixture: &Fixture,
    ledger: &InvestigationInputLedger,
    reason: Option<UnavailableReason>,
) -> Result<SourceContinuationSnapshot, InvestigationStoreError> {
    fixture
        .f
        .store
        .next_source_continuation_snapshot(&SourceContinuationSnapshotInput {
            investigation_id: &fixture.f.basis.task_id,
            expected_revision: fixture.f.task.detail.summary.revision,
            request: &fixture.f.request,
            candidate_ledger: ledger,
            outcome: reason
                .map(SourceContinuationOutcome::Unavailable)
                .unwrap_or(SourceContinuationOutcome::Acquired),
            active_milliseconds: fixture.f.task.detail.usage.active_milliseconds + 1,
        })
}

#[test]
fn source_continuation_snapshot_matches_real_prompt_and_settlement_without_writes() {
    for reason in [
        None,
        Some(UnavailableReason::PinnedTargetAbsent),
        Some(UnavailableReason::RangeOutOfBounds),
    ] {
        let mut fixture = Fixture::new();
        fixture.charge();
        let (candidate, _) = fixture.candidate(reason.clone());
        let before = encode(&fixture.f.task, MAX_RECORD_BYTES).unwrap();
        let changes = fixture.f.store.conn.total_changes();
        let projected = snapshot(&fixture, candidate.ledger(), reason.clone()).unwrap();
        #[cfg(unix)]
        {
            // AC-0327: real returned DTO serialization is a retained lower bound,
            // not a measurement of peak allocations or process RSS.
            let (retained, _) = snapshot_workspace::workspace().unwrap();
            let bytes = serde_json::to_vec(&(
                projected.request(),
                projected.base_ledger(),
                projected.ledger(),
                projected.detail(),
                projected.metadata(),
                &projected.basis().input_ledger_hash,
                &projected.basis().previous_ledger,
                projected.basis().completed_tool,
                projected.next_action_id(),
            ))
            .unwrap()
            .len();
            assert!(retained >= bytes + std::mem::size_of_val(&projected));
        }
        assert_eq!(fixture.f.store.conn.total_changes(), changes);
        let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
        let saved = load_next_prompt_task_base(&tx, &fixture.f.basis.task_id).unwrap();
        assert_eq!(encode(&saved, MAX_RECORD_BYTES).unwrap(), before);
        assert!(completion::attempts(&tx, &saved).unwrap().is_empty());
        tx.commit().unwrap();
        assert_eq!(
            projected.saved_revision(),
            fixture.f.task.detail.summary.revision
        );
        assert_eq!(
            serde_json::to_value(projected.request()).unwrap(),
            serde_json::to_value(&fixture.f.request).unwrap()
        );
        assert_eq!(projected.base_ledger(), &fixture.f.ledger);
        assert_eq!(projected.ledger(), candidate.ledger());
        assert_eq!(projected.attempts().len(), 1);
        assert_eq!(projected.attempt_ledgers(), &[candidate.ledger().clone()]);
        assert_eq!(
            projected.metadata().completed_attempt_ids,
            [projected.attempts()[0].attempt_id.clone()]
        );
        let evidence = if reason.is_none() {
            vec![agents::AgentEvidence {
                id: "evidence-1".into(),
                source: fixture.r.source.source.clone(),
                text: "private text".into(),
            }]
        } else {
            vec![]
        };
        let actual = InvestigationInput::prepare_v9_with_policy(
            &projected.request().question,
            projected.detail().summary.specialist_id,
            projected.ledger().clone(),
            &[fixture.raw()],
            &evidence,
            &[],
            &projected.detail().usage,
            projected.basis(),
            projected.request(),
            projected.metadata(),
            &[],
            projected.mode(),
            projected.capacity_refusal_count(),
            projected.capacity_feedback(),
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
        assert_eq!(
            actual
                .action(projected.next_action_id().into())
                .unwrap()
                .payload,
            candidate
                .action(projected.next_action_id().into())
                .unwrap()
                .payload
        );
        let prepared = prepared(&actual, projected.next_action_id().into());
        let continuation = SourceContinuation {
            current: &fixture.current,
            current_prepared: &fixture.prepared,
            candidate: &actual,
            candidate_prepared: &prepared,
            proof: &fixture.proof,
        };
        let outcome = match reason {
            Some(reason) => SourceSettlement::Unavailable {
                reason,
                continuation,
            },
            None => SourceSettlement::Acquired(continuation),
        };
        let committed = fixture
            .f
            .store
            .settle_next_prompt_source(
                &fixture.f._execution_guard,
                projected.saved_revision(),
                outcome,
                projected.detail().usage.active_milliseconds,
                |_| Ok(()),
            )
            .unwrap();
        let mut projected_detail = projected.detail().clone();
        // Wall-clock journal timestamps are sampled again by the commit; they
        // are not protected prompt inputs or a capability from this snapshot.
        projected_detail.summary.updated_at = committed.summary.updated_at.clone();
        assert_eq!(committed, projected_detail);
        let reopened = JobStore::open(fixture.f.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let saved = load_next_prompt_task_base(&tx, &fixture.f.basis.task_id).unwrap();
        assert_eq!(
            completion::attempts(&tx, &saved)
                .unwrap()
                .into_iter()
                .map(|entry| entry.record)
                .collect::<Vec<_>>(),
            projected.attempts()
        );
        assert!(snapshot(&fixture, candidate.ledger(), None).is_err());
    }
}

#[test]
fn source_continuation_snapshot_rejects_missing_charge_stale_request_and_candidate_basis() {
    let mut fixture = Fixture::new();
    let (candidate, _) = fixture.candidate(None);
    assert!(matches!(
        snapshot(&fixture, candidate.ledger(), None),
        Err(InvestigationStoreError::Invalid)
    ));
    fixture.charge();
    let mut request = fixture.f.request.clone();
    request.question.push_str(" altered");
    let mut input = SourceContinuationSnapshotInput {
        investigation_id: &fixture.f.basis.task_id,
        expected_revision: fixture.f.task.detail.summary.revision,
        request: &request,
        candidate_ledger: candidate.ledger(),
        outcome: SourceContinuationOutcome::Acquired,
        active_milliseconds: fixture.f.task.detail.usage.active_milliseconds + 1,
    };
    assert!(matches!(
        fixture.f.store.next_source_continuation_snapshot(&input),
        Err(InvestigationStoreError::Conflict)
    ));
    input.request = &fixture.f.request;
    input.expected_revision += 1;
    assert!(matches!(
        fixture.f.store.next_source_continuation_snapshot(&input),
        Err(InvestigationStoreError::Stale)
    ));
    let mut wrong = candidate.ledger().clone();
    wrong
        .citations
        .last_mut()
        .unwrap()
        .source
        .as_mut()
        .unwrap()
        .path = "other.ts".into();
    assert!(snapshot(&fixture, &wrong, None).is_err());
    assert!(
        snapshot(
            &fixture,
            candidate.ledger(),
            Some(UnavailableReason::PinnedTargetAbsent)
        )
        .is_err()
    );
    assert!(
        snapshot(
            &fixture,
            &fixture.f.ledger,
            Some(UnavailableReason::InventoryExceedsMaxBytes)
        )
        .is_err()
    );
}

#[test]
fn source_continuation_snapshot_rejects_cancellation_and_corrupted_saved_authority() {
    for corrupt in [false, true] {
        let mut fixture = Fixture::new();
        fixture.charge();
        let (candidate, _) = fixture.candidate(None);
        if corrupt {
            fixture
                .f
                .store
                .conn
                .execute(
                    "UPDATE investigation_tasks SET live=0 WHERE id=?1",
                    [&fixture.f.basis.task_id],
                )
                .unwrap();
            assert!(matches!(
                snapshot(&fixture, candidate.ledger(), None),
                Err(InvestigationStoreError::Invalid)
            ));
        } else {
            fixture
                .f
                .store
                .cancel_next_prompt_investigation(&fixture.f.basis.task_id)
                .unwrap();
            fixture.refresh();
            assert!(matches!(
                snapshot(&fixture, candidate.ledger(), None),
                Err(InvestigationStoreError::Cancelled)
            ));
        }
    }
}

#[test]
fn source_continuation_snapshot_rejects_checksummed_unbacked_prior_refusal() {
    let mut fixture = Fixture::new();
    fixture.charge();
    let (candidate, _) = fixture.candidate(None);
    let mut forged = RefusalRecord {
        schema_version: 1,
        refusal_id: String::new(),
        task_id: fixture.f.basis.task_id.clone(),
        request_hash: fixture.f.metadata.request_hash.clone(),
        obligations_hash: fixture.f.metadata.obligations_hash.clone(),
        step_id: "uninvoked-prior-finish".into(),
        input_ledger_hash: fixture.f.basis.input_ledger_hash.clone(),
        disclosure_hash: fixture
            .f
            .step
            .disclosure
            .as_ref()
            .unwrap()
            .fingerprint_with_policy(&fixture.f.ledger, &INVESTIGATION_V2_POLICY)
            .unwrap(),
        finish_hash: core_prov::content_hash(b"no corresponding saved finish"),
        outcome: CompletionOutcome::Supported,
        response_hash: fixture.response().response_hash,
        previous_refusal: None,
        refusal_count: 1,
        assessment: vec![],
        feedback: RefusalFeedback {
            schema_version: 1,
            unmet: vec![],
        },
    };
    forged.refusal_id = forged.content_id().unwrap();
    let tx = fixture.f.store.conn.transaction().unwrap();
    // This fixture has never finished, so the optional refusal namespace is
    // absent. Create its genuine DDL before injecting the adversarial row.
    staged::initialize(&tx).unwrap();
    insert_body(
        &tx,
        "staged_completion_refusals",
        &fixture.f.basis.task_id,
        &1i64,
        &forged,
        MAX_MANIFEST_BYTES,
    )
    .unwrap();
    tx.commit().unwrap();
    // An adversarial canonical row has a valid content ID and storage checksum,
    // but no genuine invoked finish/assessment history. Neither is authority.
    let changes = fixture.f.store.conn.total_changes();
    assert!(snapshot(&fixture, candidate.ledger(), None).is_err());
    assert_eq!(fixture.f.store.conn.total_changes(), changes);
}
