//! AC-0287: replay reuse preserves every historical cutoff and corruption check.
use super::*;
use completion::FinishFixture;

#[cfg(unix)]
#[test]
fn saved_metadata_history_workspace_includes_completion_refusal_feedback() {
    let mut f = FinishFixture::new(false);
    f.begin();
    assert_eq!(f.publish().unwrap().completion.unwrap().refusal_count, 1);
    f.next();
    f.begin();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let history = SavedMetadataHistory::load(&tx, &f.base.f.task).unwrap();
    assert_eq!(history.refusals.len(), 1);
    let step = &history.steps.last().unwrap().1;
    let metadata = history.metadata_at(&step.step_id).unwrap();
    assert_eq!(metadata.refusal_count, 1);
    assert_eq!(
        metadata,
        metadata_at_saved_step(&tx, &f.base.f.task, step).unwrap()
    );
    let (retained, project, peak) = next_prompt_metadata_workspace().unwrap();
    let refusal_bytes = serde_json::to_vec(&history.refusals).unwrap().len();
    assert!(retained >= refusal_bytes);
    assert!(project >= serde_json::to_vec(&metadata).unwrap().len());
    assert!(peak >= retained.checked_add(project).unwrap());
}

#[test]
fn saved_metadata_history_matches_standalone_cutoffs_with_one_sweep_per_timeline() {
    let mut f = FinishFixture::new(true);
    f.capacity_refusal();
    f.capacity_refusal();
    f.begin();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let task = &f.base.f.task;
    let history = SavedMetadataHistory::load(&tx, task).unwrap();
    assert_eq!(history.steps.len(), 4);
    // AC-0327: genuine history keeps all of these owners live during projection.
    #[cfg(unix)]
    let projection_bound = {
        let (retained, project, peak) = next_prompt_metadata_workspace().unwrap();
        let bytes = serde_json::to_vec(&history.steps).unwrap().len()
            + serde_json::to_vec(&history.ordinals).unwrap().len()
            + serde_json::to_vec(&history.attempts).unwrap().len()
            + serde_json::to_vec(&history.refusals).unwrap().len()
            + serde_json::to_vec(&history.archive).unwrap().len();
        assert!(retained >= bytes);
        assert!(peak >= retained.checked_add(project).unwrap());
        project
    };
    for (_, step) in &history.steps {
        #[cfg(unix)]
        assert!(
            projection_bound
                >= serde_json::to_vec(&history.metadata_at(&step.step_id).unwrap())
                    .unwrap()
                    .len()
        );
        assert_eq!(
            history.metadata_at(&step.step_id).unwrap(),
            metadata_at_saved_step(&tx, task, step).unwrap()
        );
    }
    assert!(matches!(
        history.metadata_at("missing"),
        Err(InvestigationStoreError::Missing)
    ));
    INVOCATION_SWEEPS.with(|count| count.set(0));
    let state = admission_journal::derive_current(&tx, task).unwrap();
    assert_eq!(state.count, 2);
    assert!(state.feedback.is_some());
    INVOCATION_SWEEPS.with(|count| assert_eq!(count.get(), 1));
    let call = task
        .admission_state
        .as_ref()
        .unwrap()
        .current_call
        .as_ref()
        .unwrap();
    let actual = admission_contract::load_saved_call_basis_core(&tx, task, call).unwrap();
    let reused = history
        .validate_timeline_call(call, &actual.ledger)
        .unwrap();
    assert_eq!(reused.step, actual.step);
    assert_eq!(reused.ledger, actual.ledger);
    assert_eq!(reused.metadata, actual.metadata);
    assert_eq!(reused.ordinal, actual.ordinal);
    #[cfg(unix)]
    {
        let (retained, peak) = next_prompt_saved_call_workspace().unwrap();
        let core_bytes = serde_json::to_vec(&actual.step).unwrap().len()
            + serde_json::to_vec(&actual.ledger).unwrap().len()
            + serde_json::to_vec(&actual.metadata).unwrap().len();
        assert!(retained >= core_bytes);
        // Originals and returned clones coexist during timeline validation.
        let (_, _, history_peak) = next_prompt_metadata_workspace().unwrap();
        assert!(
            peak >= history_peak
                .checked_add(retained.checked_mul(2).unwrap())
                .unwrap()
        );
    }
}

#[test]
fn saved_metadata_history_write_rollback_and_ddl_invalidate_same_transaction_view() {
    let mut f = FinishFixture::new(false);
    f.begin();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let task = &f.base.f.task;
    let history = SavedMetadataHistory::load(&tx, task).unwrap();
    tx.execute_batch("SAVEPOINT mutation").unwrap();
    tx.execute(
        "UPDATE jobs SET status=status WHERE id=?1",
        [task.detail.summary.job_id],
    )
    .unwrap();
    tx.execute_batch("ROLLBACK TO mutation; RELEASE mutation")
        .unwrap();
    assert!(matches!(
        history.metadata_at("step-1"),
        Err(InvestigationStoreError::Stale)
    ));
    let history = SavedMetadataHistory::load(&tx, task).unwrap();
    tx.execute_batch("CREATE TEMP TABLE stale_history_marker (id INTEGER)")
        .unwrap();
    assert!(matches!(
        history.metadata_at("step-1"),
        Err(InvestigationStoreError::Stale)
    ));
    let history = SavedMetadataHistory::load(&tx, task).unwrap();
    // Main DDL must also invalidate without an affected row/body count.
    tx.execute_batch("CREATE TABLE stale_main_history_marker (id INTEGER)")
        .unwrap();
    assert!(matches!(
        history.metadata_at("step-1"),
        Err(InvestigationStoreError::Stale)
    ));
}

#[test]
fn saved_metadata_history_fresh_replay_rejects_rehashed_earlier_call_at_same_row_count() {
    let mut f = FinishFixture::new(false);
    f.capacity_refusal();
    f.begin();
    let id = f.base.id().to_owned();
    {
        let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
        let history = SavedMetadataHistory::load(&tx, &f.base.f.task).unwrap();
        let mut first = history.step("step-1").unwrap().clone();
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM investigation_invocations WHERE task_id=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        first
            .disclosure
            .as_mut()
            .unwrap()
            .admission
            .as_mut()
            .unwrap()
            .capacity_refusal_count = 1;
        let bytes = encode(&first, MAX_INVOCATION_BYTES).unwrap();
        let trigger: String = tx
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='investigation_invocations_no_update'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        tx.execute_batch("DROP TRIGGER investigation_invocations_no_update")
            .unwrap();
        tx.execute("UPDATE investigation_invocations SET body=?2,body_hash=?3 WHERE task_id=?1 AND step_id='step-1'",params![id,bytes,core_prov::content_hash(&bytes)]).unwrap();
        tx.execute_batch(&trigger).unwrap();
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM investigation_invocations WHERE task_id=?1",
                [&id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            count
        );
        assert!(matches!(
            history.metadata_at("step-1"),
            Err(InvestigationStoreError::Stale)
        ));
        drop(history);
        tx.commit().unwrap();
    }
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    assert!(admission_journal::derive_current(&tx, &f.base.f.task).is_err());
    assert!(load_next_prompt_task(&tx, &id).is_err());
}

#[test]
fn saved_metadata_history_reused_append_core_rejects_substituted_call_basis() {
    let mut f = completion::internal_next_prompt_fixture();
    let proposal = admission_journal::tests::proposal(&f, 12);
    let response = admission_journal::tests::response(&f, &proposal);
    let tx = f.store.conn.transaction().unwrap();
    admission_journal::initialize(&tx).unwrap();
    insert_body(
        &tx,
        "investigation_responses",
        &f.basis.task_id,
        &f.basis.step_id,
        &response,
        4096,
    )
    .unwrap();
    let mut changed = f.basis.clone();
    changed.completion_metadata_hash = "f".repeat(64);
    f.task.admission_state.as_mut().unwrap().current_call = Some(changed.clone());
    assert_eq!(
        admission_journal::append(
            &tx,
            &f.task,
            &changed,
            &response,
            &proposal,
            CapacityRefusalReason::PromptCapacity
        ),
        Err(InvestigationStoreError::Invalid)
    );
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM investigation_admission_refusals",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn saved_acquisition_authority_rederives_exact_historical_cutoffs() {
    // AC-0483: real synthetic inventory admission, then explicit gated-family
    // witness conversion. This is not an @18 producer, migration or model run.
    use agents::investigation::{AcquisitionAdmission, acquisition_choices::AcquisitionChoices};
    use completion::next_prompt_snapshot::tests::rewrite;
    for role in [
        SpecialistId::DomainAnalystV15,
        SpecialistId::DomainAnalystV17,
        SpecialistId::DomainAnalystV18,
    ] {
        let mut fixture = FinishFixture::new(true);
        fixture.begin();
        let f = &mut fixture.base.f;
        let policy = &INVESTIGATION_V2_POLICY;
        let id = f.task.detail.summary.investigation_id.clone();
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let history = SavedMetadataHistory::load(&tx, &f.task).unwrap();
        assert_eq!(history.attempts.len(), 1);
        let mut entry: AttemptEntry = read_body(&tx, "completion_attempts", "sequence", &id, &1i64)
            .unwrap()
            .unwrap();
        let mut witnesses = history
            .steps
            .iter()
            .map(|(ordinal, step)| {
                let ledger = ledger_at(&tx, &id, u64::from(*ordinal - 1)).unwrap();
                assert_eq!(
                    ledger.fingerprint_with_policy(policy).unwrap(),
                    step.input_ledger_hash
                );
                (
                    *ordinal,
                    step.clone(),
                    ledger,
                    history.metadata_at(&step.step_id).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(witnesses.len(), 2);
        drop(history);
        f.request.specialist_id = serde_json::to_value(role).unwrap().as_str().unwrap().into();
        let intent = CompletionIntent::new(&f.request).unwrap();
        let old_attempt = entry.record.attempt_id.clone();
        entry.record.request_hash = intent.request_hash.clone();
        entry.record.attempt_id = entry.record.content_id().unwrap();
        rewrite(
            &tx,
            "completion_attempts",
            &id,
            "sequence",
            &1i64,
            &encode(&entry, MAX_RECORD_BYTES).unwrap(),
        );
        f.task.intent_hash = intent.request_hash.clone();
        f.task.detail.completion.as_mut().unwrap().request_hash = intent.request_hash.clone();
        f.task.completion_intent = Some(intent.clone());
        f.task.detail.summary.specialist_id = role;
        f.task.detail.specialist = role.definition().unwrap();
        let attempt_ledger = ledger_at(&tx, &id, entry.record.input_revision).unwrap();
        let mut calls = vec![];
        for (ordinal, step, ledger, metadata) in &mut witnesses {
            metadata.request_hash = intent.request_hash.clone();
            for attempt in &mut metadata.completed_attempt_ids {
                assert_eq!(attempt, &old_attempt);
                *attempt = entry.record.attempt_id.clone();
            }
            let evidence = [agents::investigation::staged_attempt::AttemptEvidence {
                record: &entry.record,
                ledger: &attempt_ledger,
            }];
            let choices = AcquisitionChoices::derive(
                &intent.contract(),
                metadata,
                &id,
                ledger,
                if *ordinal == 1 { &[] } else { &evidence },
                policy,
            )
            .unwrap();
            assert_eq!(choices.attempts_complete(), *ordinal == 2);
            let disclosure = step.disclosure.as_mut().unwrap();
            disclosure.completion_contract_hash =
                Some(intent.contract().fingerprint_with_policy(policy).unwrap());
            disclosure.completion_metadata_hash =
                Some(metadata.fingerprint_with_policy(policy).unwrap());
            let history = agents::investigation::ProgressiveInvestigationContext::prepare(
                &intent.request_hash,
                ledger,
                &[],
                policy,
            )
            .unwrap()
            .query_history;
            let query_history = disclosure.query_history.as_mut().unwrap();
            query_history.request_hash = intent.request_hash.clone();
            query_history.registry_hash = history.registry_hash;
            let admission = disclosure.admission.as_mut().unwrap();
            admission.acquisition =
                (role == SpecialistId::DomainAnalystV18).then(|| AcquisitionAdmission {
                    choices_hash: core_prov::content_hash(
                        &encode(&choices, MAX_ACTION_BYTES).unwrap(),
                    ),
                    action_feedback_hash: None,
                });
            let call = admission_contract::SavedCallBasis {
                schema_version: 1,
                task_id: id.clone(),
                request_hash: intent.request_hash.clone(),
                step_id: step.step_id.clone(),
                invocation_ordinal: *ordinal,
                input_ledger_hash: step.input_ledger_hash.clone(),
                input_revision: ledger.revision,
                completion_metadata_hash: metadata.fingerprint_with_policy(policy).unwrap(),
                admission: admission.clone(),
            };
            rewrite(
                &tx,
                "investigation_invocations",
                &id,
                "step_id",
                &step.step_id,
                &encode(step, MAX_INVOCATION_BYTES).unwrap(),
            );
            calls.push(call);
        }
        f.task.pending = Some(witnesses[1].1.clone());
        f.task.admission_state.as_mut().unwrap().current_call = Some(calls[1].clone());
        admission_journal::tests::save_task(&tx, &f.task);
        tx.commit().unwrap();
        // Reopen after publication of the later read, including the earlier cutoff.
        let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
        for index in 0..2 {
            let tx = reopened.conn.unchecked_transaction().unwrap();
            let task = load_next_prompt_task(&tx, &id).unwrap();
            let core =
                admission_contract::load_saved_call_basis_core(&tx, &task, &calls[index]).unwrap();
            assert_eq!(core.metadata.completed_attempt_ids.len(), index);
            assert_eq!(core.ledger, witnesses[index].2);
        }
        for mutation in ["choices", "feedback", "ledger", "attempt"] {
            if role != SpecialistId::DomainAnalystV18 && matches!(mutation, "choices" | "feedback")
            {
                continue;
            }
            let tx = reopened.conn.unchecked_transaction().unwrap();
            let task = load_next_prompt_task(&tx, &id).unwrap();
            let mut call = calls[0].clone();
            let mut step = witnesses[0].1.clone();
            match mutation {
                "choices" | "feedback" => {
                    let stamp = call.admission.acquisition.as_mut().unwrap();
                    if mutation == "choices" {
                        stamp.choices_hash = "f".repeat(64);
                    } else {
                        stamp.action_feedback_hash = Some("f".repeat(64));
                    }
                    step.disclosure.as_mut().unwrap().admission = Some(call.admission.clone());
                    rewrite(
                        &tx,
                        "investigation_invocations",
                        &id,
                        "step_id",
                        &step.step_id,
                        &encode(&step, MAX_INVOCATION_BYTES).unwrap(),
                    );
                }
                "ledger" => {
                    let mut ledger = witnesses[0].2.clone();
                    ledger.revision = 1; // valid body under the wrong retained key
                    rewrite(
                        &tx,
                        "investigation_inputs",
                        &id,
                        "revision",
                        &0i64,
                        &encode(&ledger, MAX_MANIFEST_BYTES).unwrap(),
                    );
                }
                "attempt" => {
                    let mut forged = entry.clone();
                    forged.record.request_hash = "f".repeat(64);
                    forged.record.attempt_id = forged.record.content_id().unwrap();
                    rewrite(
                        &tx,
                        "completion_attempts",
                        &id,
                        "sequence",
                        &1i64,
                        &encode(&forged, MAX_RECORD_BYTES).unwrap(),
                    );
                }
                _ => unreachable!(),
            }
            assert!(
                admission_contract::load_saved_call_basis_core(&tx, &task, &call).is_err(),
                "{mutation}"
            );
            assert!(load_next_prompt_task(&tx, &id).is_err(), "{mutation}");
            // Dropping each transaction preserves the independently passing control.
        }
        #[cfg(unix)]
        {
            let transient = metadata_history_workspace::acquisition_choices_workspace().unwrap();
            let (_, _, history) = next_prompt_metadata_workspace().unwrap();
            assert!(next_prompt_saved_call_workspace().unwrap().1 >= history + transient);
        }
    }
}
