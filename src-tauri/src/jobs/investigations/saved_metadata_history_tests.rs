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
                    capacity_exhausted: false,
                    finish_reason: None,
                    choices_hash: choices.stamp_hash().unwrap(),
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
            if role == SpecialistId::DomainAnalystV18 {
                INVOCATION_SWEEPS.with(|count| count.set(0));
            }
            let core =
                admission_contract::load_saved_call_basis_core(&tx, &task, &calls[index]).unwrap();
            if role == SpecialistId::DomainAnalystV18 {
                // AC-0489/#1573: preserve every historical cutoff, including
                // one before the real inventory attempt, with one replay.
                let expected =
                    admission_journal::derive_at(&tx, &task, &calls[index].step_id).unwrap();
                INVOCATION_SWEEPS.with(|count| assert_eq!(count.get(), 2));
                INVOCATION_SWEEPS.with(|count| count.set(0));
                let (paired, capacity) =
                    admission_contract::load_saved_call_basis_core_and_capacity(
                        &tx,
                        &task,
                        &calls[index],
                    )
                    .unwrap();
                INVOCATION_SWEEPS.with(|count| assert_eq!(count.get(), 1));
                assert_eq!(paired.step, core.step);
                assert_eq!(paired.ledger, core.ledger);
                assert_eq!(paired.metadata, core.metadata);
                assert_eq!(paired.ordinal, core.ordinal);
                assert_eq!(capacity, expected);
                let mut foreign = calls[index].clone();
                foreign.step_id = "foreign-step".into();
                assert!(matches!(
                    admission_contract::load_saved_call_basis_core_and_capacity(
                        &tx, &task, &foreign,
                    ),
                    Err(InvestigationStoreError::Invalid)
                ));
                foreign.invocation_ordinal = u32::MAX;
                assert!(matches!(
                    admission_contract::load_saved_call_basis_core_and_capacity(
                        &tx, &task, &foreign,
                    ),
                    Err(InvestigationStoreError::Missing)
                ));
            }
            assert_eq!(core.metadata.completed_attempt_ids.len(), index);
            assert_eq!(core.ledger, witnesses[index].2);
            if role == SpecialistId::DomainAnalystV18 {
                assert!(next_prompt_invocations(&tx, &task).is_ok()); // full prefix authentication
            }
        }
        for mutation in ["choices", "feedback", "ledger", "attempt"] {
            if role != SpecialistId::DomainAnalystV18 && matches!(mutation, "choices" | "feedback")
            {
                continue;
            }
            let tx = reopened.conn.unchecked_transaction().unwrap();
            let mut task = load_next_prompt_task(&tx, &id).unwrap();
            let index = usize::from(mutation == "choices");
            let mut call = calls[index].clone();
            let mut step = witnesses[index].1.clone();
            if mutation == "choices" {
                validate_step_basis(&tx, &task, &step).unwrap();
            }
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
            if mutation == "choices" {
                task.pending = Some(step.clone());
                task.admission_state.as_mut().unwrap().current_call = Some(call.clone());
                admission_journal::tests::save_task(&tx, &task);
                assert!(validate_step_basis(&tx, &task, &step).is_err());
                assert!(next_prompt_invocations(&tx, &task).is_err());
            }
            assert!(
                admission_contract::load_saved_call_basis_core(&tx, &task, &call).is_err(),
                "{mutation}"
            );
            if role == SpecialistId::DomainAnalystV18 {
                assert!(
                    admission_contract::load_saved_call_basis_core_and_capacity(&tx, &task, &call,)
                        .is_err(),
                    "paired {mutation}"
                );
            }
            assert!(load_next_prompt_task(&tx, &id).is_err(), "{mutation}");
            // Dropping each transaction preserves the independently passing control.
        }
        if role == SpecialistId::DomainAnalystV18 {
            // Settle through the existing capacity writer: Ready has no current
            // call, so public history must independently validate older stamps.
            let proposal = serde_json::json!({"type":"query_context", "context_id": f.request.expected_context_id,
                "query":{"scope":InvestigationScope::All,"kind":null,"labels":[],"max_facts":1,"max_bytes":16384,"cursor":null}});
            let mut response = fixture.response.clone();
            response.action_hash =
                core_prov::content_hash(&encode(&proposal, MAX_ACTION_BYTES).unwrap());
            let tx = reopened.conn.unchecked_transaction().unwrap();
            let ready = admission_transition::refuse_capacity(
                &tx,
                &f.task,
                &response,
                &proposal,
                CapacityRefusalReason::PromptCapacity,
            )
            .unwrap();
            assert_eq!(ready.phase, Phase::Ready);
            assert!(
                ready
                    .admission_state
                    .as_ref()
                    .unwrap()
                    .current_call
                    .is_none()
            );
            tx.commit().unwrap();
            let tx = reopened.conn.unchecked_transaction().unwrap();
            load_next_prompt_task(&tx, &id).unwrap(); // positive Ready control
            let mut historical = witnesses[0].1.clone();
            historical
                .disclosure
                .as_mut()
                .unwrap()
                .admission
                .as_mut()
                .unwrap()
                .acquisition
                .as_mut()
                .unwrap()
                .choices_hash = "f".repeat(64);
            rewrite(
                &tx,
                "investigation_invocations",
                &id,
                "step_id",
                &historical.step_id,
                &encode(&historical, MAX_INVOCATION_BYTES).unwrap(),
            );
            assert!(
                load_next_prompt_task(&tx, &id).is_err(),
                "Ready must authenticate prior choices"
            );
        }
        #[cfg(unix)]
        {
            let transient = metadata_history_workspace::acquisition_choices_workspace().unwrap();
            let (_, _, history) = next_prompt_metadata_workspace().unwrap();
            assert!(next_prompt_saved_call_workspace().unwrap().1 >= history + transient);
        }
    }
}

// AC-0489/#1573: genuine initial writer and real acquired inventory; the saved
// call validator reuses one full replay without accepting caller-made authority.
#[test]
fn paired_saved_call_validation_uses_one_authenticated_sweep() {
    use completion::next_prompt_snapshot::tests::fixture::Fixture;
    let mut fixture = Fixture::customized(false, |f| {
        f.request.specialist_id = "domain-analyst@18".into();
        let intent = completion::host_fixture_intent(&f.request).unwrap();
        f.task.intent_hash = intent.request_hash.clone();
        f.task.detail.completion = Some(completion::initial_detail(&intent));
        f.task.completion_intent = Some(intent);
        f.task.detail.summary.specialist_id = SpecialistId::DomainAnalystV18;
        f.task.detail.specialist = SpecialistId::DomainAnalystV18.definition().unwrap();
        f.metadata = completion::derive_protected_metadata(&f.task, vec![], &[]).unwrap();
        let tx = f.store.conn.unchecked_transaction().unwrap();
        admission_journal::tests::save_task(&tx, &f.task);
        tx.commit().unwrap();
    });
    let mut saved = None;
    fixture.acquire_inventory_observed(|fixture| {
        let task = &fixture.f.task;
        let call = task
            .admission_state
            .as_ref()
            .unwrap()
            .current_call
            .as_ref()
            .unwrap();
        let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
        let changes = tx.total_changes();
        INVOCATION_SWEEPS.with(|count| count.set(0));
        call.validate_saved(&tx, task).unwrap();
        INVOCATION_SWEEPS.with(|count| assert_eq!(count.get(), 1));
        assert_eq!(tx.total_changes(), changes);
        saved = Some(call.clone());
    });
    let call = saved.unwrap();
    // A later transaction must authenticate fresh rows; no replay cache escapes
    // the paired call. The historical cutoff still excludes the later attempt.
    let tx = fixture.f.store.conn.unchecked_transaction().unwrap();
    let task = &fixture.f.task;
    let core = admission_contract::load_saved_call_basis_core(&tx, task, &call).unwrap();
    let expected = admission_journal::derive_at(&tx, task, &call.step_id).unwrap();
    let (paired, capacity) =
        admission_contract::load_saved_call_basis_core_and_capacity(&tx, task, &call).unwrap();
    assert_eq!(paired.ledger, core.ledger);
    assert_eq!(paired.metadata, core.metadata);
    assert_eq!(capacity, expected);
    assert!(paired.metadata.completed_attempt_ids.is_empty());
    tx.commit().unwrap();
    assert!(matches!(
        admission_contract::load_saved_call_basis_core_and_capacity(
            &fixture.f.store.conn,
            task,
            &call,
        ),
        Err(InvestigationStoreError::Invalid)
    ));
}

// AC-0489: actual canonical row reads, same-transaction parity and hash corruption.
#[test]
fn saved_metadata_history_reuses_invocation_rows_and_rejects_hash_substitution() {
    let mut fixture = FinishFixture::new(true);
    fixture.begin();
    let f = &fixture.base.f;
    completion::assert_attempt_invocation_reuse(&f.store.conn, &f.task);
    let tx = f.store.conn.unchecked_transaction().unwrap();
    completion::assert_attempt_invocation_reuse(&tx, &f.task);
    let id = &f.task.detail.summary.investigation_id;
    let entries = completion::attempts(&tx, &f.task).unwrap();
    assert!(matches!(
        entries[0].record.outcome,
        AttemptOutcome::InventoryAcquired { .. }
    ));
    let mut step: PendingInvestigationStep = read_body(
        &tx,
        "investigation_invocations",
        "step_id",
        id,
        &entries[0].record.step_id,
    )
    .unwrap()
    .unwrap();
    step.input_ledger_hash = "0".repeat(64);
    step.disclosure.as_mut().unwrap().input_ledger_hash = step.input_ledger_hash.clone();
    validate_step(&step, &f.task.detail.limits).unwrap();
    completion::rewrite_test_body(
        &tx,
        "investigation_invocations",
        id,
        "step_id",
        &step.step_id,
        &encode(&step, MAX_RECORD_BYTES).unwrap(),
    );
    completion::assert_attempt_invocation_refusal(&tx, &f.task);
}
