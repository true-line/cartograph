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
            refusal: None,
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
            size_binding: None,
            current: &fixture.current,
            current_prepared: &fixture.prepared,
            candidate: &actual,
            candidate_prepared: &prepared,
            proof: &fixture.proof,
            refusal: None,
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
        refusal: None,
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
            citation_drop: None,
            prose_correction: None,
            schema_version: 1,
            citation_correction: None,
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

fn sealed_admission(
    input: &InvestigationInput,
    prepared: &PreparedBoundedCompletion,
) -> DisclosedAdmissionState {
    input
        .seal_disclosure_with_policy(prepared, &INVESTIGATION_V2_POLICY)
        .unwrap()
        .unwrap()
        .admission
        .unwrap()
}
fn source_rows(fixture: &Fixture) -> Vec<Vec<u8>> {
    let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &fixture.f.basis.task_id).unwrap();
    let mut rows = vec![encode(&task, MAX_RECORD_BYTES).unwrap()];
    for table in [
        "investigation_inputs",
        "investigation_events",
        "completion_attempts",
        "investigation_read_reservations",
        "investigation_read_validation_charges",
        "investigation_read_dispositions",
    ] {
        rows.extend(
            tx.prepare(&format!(
                "SELECT body FROM {table} WHERE task_id=?1 ORDER BY body"
            ))
            .unwrap()
            .query_map([&fixture.f.basis.task_id], |row| row.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(Result::unwrap),
        );
    }
    rows
}

// AC-0487: genuine saved invocation, read-only candidate, real seal, owner writer
// and fresh-open reader. The native outcome is scripted, with no source/provider I/O.
#[test]
fn source_prospective_attachment_matches_outcomes_and_reopens() {
    for reason in [
        None,
        Some(UnavailableReason::PinnedTargetAbsent),
        Some(UnavailableReason::RangeOutOfBounds),
    ] {
        let mut fixture = Fixture::gated();
        fixture.charge();
        let before = source_rows(&fixture);
        let changes = fixture.f.store.conn.total_changes();
        let (candidate, prepared) = fixture.candidate(reason.clone());
        let projection = snapshot(&fixture, candidate.ledger(), reason.clone()).unwrap();
        assert_eq!(source_rows(&fixture), before);
        assert_eq!(fixture.f.store.conn.total_changes(), changes);
        let admission = sealed_admission(&candidate, &prepared);
        assert_ne!(
            admission.acquisition,
            sealed_admission(&fixture.current, &fixture.prepared).acquisition
        );
        assert!(
            admission
                .acquisition
                .as_ref()
                .unwrap()
                .action_feedback_hash
                .is_none()
        );
        let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
        let choices = completion::SavedMetadataHistory::load(&tx, &fixture.f.task)
            .unwrap()
            .prospective_choices(
                candidate.ledger(),
                projection.metadata(),
                projection.attempts(),
            )
            .unwrap()
            .unwrap();
        validate_acquisition(&choices, &admission, &INVESTIGATION_V2_POLICY).unwrap();
        #[cfg(unix)]
        {
            let retained = snapshot_workspace::workspace().unwrap().0;
            let wire = serde_json::to_vec(&(
                projection.request(),
                projection.base_ledger(),
                projection.ledger(),
                projection.detail(),
                projection.metadata(),
                &projection.basis().input_ledger_hash,
                &projection.basis().previous_ledger,
                projection.basis().completed_tool,
                &*choices,
            ))
            .unwrap()
            .len();
            assert!(retained >= wire + std::mem::size_of_val(&projection));
        }
        assert!(
            choices
                .entries()
                .iter()
                .all(|entry| entry.obligation_id != projection.attempts()[0].obligation_id)
        );
        assert!(!choices.entries().is_empty()); // inventory remains unattempted
        let matching = fixture
            .f
            .request
            .obligations
            .obligations
            .iter()
            .filter(|item| fixture.r.binding.original_read.matches(item))
            .map(|item| item.key().1)
            .collect::<Vec<_>>();
        assert_eq!(
            projection
                .attempts()
                .iter()
                .map(|item| item.obligation_id.as_str())
                .collect::<Vec<_>>(),
            matching
        );
        drop(tx);
        let calls = std::cell::Cell::new(0);
        let before_usage = fixture.f.task.detail.usage.clone();
        fixture
            .f
            .store
            .publish_next_prompt_source(
                &fixture.f._execution_guard,
                projection.saved_revision(),
                SourceContinuation {
                    size_binding: None,
                    current: &fixture.current,
                    current_prepared: &fixture.prepared,
                    candidate: &candidate,
                    candidate_prepared: &prepared,
                    proof: &fixture.proof,
                    refusal: None,
                },
                reason
                    .clone()
                    .map(SourceContinuationOutcome::Unavailable)
                    .unwrap_or(SourceContinuationOutcome::Acquired),
                projection.detail().usage.active_milliseconds,
                |_| {
                    calls.set(calls.get() + 1);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(calls.get(), 1);
        // AC-0489: acquired and unavailable source outcomes preserve exact reread results.
        {
            let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
            let task = load_next_prompt_task(&tx, &fixture.f.basis.task_id).unwrap();
            let attempts = completion::attempts(&tx, &task).unwrap();
            assert!(attempts.iter().any(|entry| if reason.is_none() {
                matches!(entry.record.outcome, AttemptOutcome::SourceAcquired { .. })
            } else {
                matches!(entry.record.outcome, AttemptOutcome::Unavailable { .. })
            }));
            completion::assert_attempt_invocation_reuse(&tx, &task);
        }
        let reopened = JobStore::open(fixture.f.directory.path().join("state.db")).unwrap();
        let saved = reopened
            .investigation_next_prompt_snapshot(&fixture.f.basis.task_id, &fixture.f.request)
            .unwrap();
        assert_eq!(saved.ledger(), candidate.ledger());
        assert_eq!(saved.metadata(), projection.metadata());
        assert_eq!(saved.attempts(), projection.attempts());
        assert_eq!(saved.attempt_ledgers(), projection.attempt_ledgers());
        let pending = saved.attach_acquisition(candidate.clone()).unwrap();
        assert_eq!(
            sealed_admission(&pending, &prepared).acquisition,
            admission.acquisition
        );
        assert_eq!(
            saved.detail().usage.evidence_items,
            before_usage.evidence_items + usize::from(reason.is_none())
        );
        assert_eq!(
            saved.ledger().revision,
            fixture.f.ledger.revision + u64::from(reason.is_none())
        );
        assert_eq!(
            saved.ledger().citations.len(),
            fixture.f.ledger.citations.len() + usize::from(reason.is_none())
        );
        assert_eq!(
            saved.detail().usage.tool_actions,
            before_usage.tool_actions + 1
        );
        assert_eq!(
            saved.detail().usage.evidence_requests,
            before_usage.evidence_requests + 1
        );
    }
}

#[test]
fn source_prospective_attachment_rejects_reason_swap_before_retained_callback() {
    let mut fixture = Fixture::gated();
    fixture.charge();
    let a = UnavailableReason::PinnedTargetAbsent;
    let b = UnavailableReason::RangeOutOfBounds;
    let (candidate_a, prepared_a) = fixture.candidate(Some(a));
    let (candidate_b, prepared_b) = fixture.candidate(Some(b.clone()));
    assert_eq!(candidate_a.ledger(), candidate_b.ledger());
    let projected_b = snapshot(&fixture, candidate_b.ledger(), Some(b.clone())).unwrap();
    let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
    let expected_b = completion::SavedMetadataHistory::load(&tx, &fixture.f.task)
        .unwrap()
        .prospective_choices(
            candidate_b.ledger(),
            projected_b.metadata(),
            projected_b.attempts(),
        )
        .unwrap()
        .unwrap();
    validate_acquisition(
        &expected_b,
        &sealed_admission(&candidate_b, &prepared_b),
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    assert!(
        validate_acquisition(
            &expected_b,
            &sealed_admission(&candidate_a, &prepared_a),
            &INVESTIGATION_V2_POLICY
        )
        .is_err()
    );
    drop(tx);
    let before = source_rows(&fixture);
    let changes = fixture.f.store.conn.total_changes();
    let calls = std::cell::Cell::new(0);
    let result = fixture.f.store.publish_next_prompt_source(
        &fixture.f._execution_guard,
        projected_b.saved_revision(),
        SourceContinuation {
            size_binding: None,
            current: &fixture.current,
            current_prepared: &fixture.prepared,
            candidate: &candidate_a,
            candidate_prepared: &prepared_a,
            proof: &fixture.proof,
            refusal: None,
        },
        SourceContinuationOutcome::Unavailable(b),
        projected_b.detail().usage.active_milliseconds,
        |_| {
            calls.set(calls.get() + 1);
            Ok(())
        },
    );
    assert!(matches!(result, Err(InvestigationStoreError::Invalid)));
    assert_eq!(calls.get(), 0);
    assert_eq!(fixture.f.store.conn.total_changes(), changes);
    assert_eq!(source_rows(&fixture), before);
    // The current seal is valid authority for its own metadata, but cannot be
    // attached as authority for the different prospective completion metadata.
    let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
    let current = completion::SavedMetadataHistory::load(&tx, &fixture.f.task)
        .unwrap()
        .acquisition_choices_at(&fixture.f.basis, &fixture.f.ledger)
        .unwrap()
        .unwrap();
    assert!(
        candidate_b
            .with_acquisition_admission(std::sync::Arc::new(current), None)
            .is_err()
    );
    drop(tx);
    assert_eq!(source_rows(&fixture), before);
}

// AC-0487: borrowing an authenticated core for sizing is not permission to
// substitute a different call or ledger. The ordinary wrapper stays strict.
#[test]
fn source_envelope_borrowed_core_keeps_exact_call_and_ledger_binding() {
    let fixture = Fixture::gated();
    let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
    let core =
        admission_contract::load_saved_call_basis_core(&tx, &fixture.f.task, &fixture.f.basis)
            .unwrap();
    let response: InvestigationResponseMeta = read_body(
        &tx,
        "investigation_responses",
        "step_id",
        &fixture.f.basis.task_id,
        &fixture.f.step.step_id,
    )
    .unwrap()
    .unwrap();
    let strict = envelope(&tx, &fixture.f.task, &fixture.r, &response).unwrap();
    assert_eq!(
        envelope_with_authenticated_core(&tx, &fixture.f.task, &fixture.r, &response, &core)
            .unwrap(),
        strict,
    );
    for wrong_step in [false, true] {
        let mut altered = admission_contract::SavedCallCore {
            step: core.step.clone(),
            ledger: core.ledger.clone(),
            metadata: core.metadata.clone(),
            ordinal: core.ordinal,
        };
        if wrong_step {
            altered.step.step_id.push_str(":foreign");
        } else {
            altered.ledger.revision += 1;
        }
        assert!(envelope_with_authenticated_core(
            &tx, &fixture.f.task, &fixture.r, &response, &altered,
        ).is_err());
    }
}
