//! AC-0287: actual complete-response decisions from genuine production starts.
use super::*;
use crate::jobs::investigations::chat::StoredChatMessage;
#[path = "next_prompt_finish_fixture.rs"]
pub(super) mod fixture;
use fixture::Fixture;

#[path = "next_prompt_finish_only_tests.rs"]
mod finish_only;

#[path = "next_prompt_invalid_finish_tests.rs"]
mod invalid_finish;

#[path = "next_prompt_citation_correction_tests.rs"]
mod citation_correction;

#[test]
fn next_prompt_finish_boundary_accepted_result7_reopens_and_rejects_family_downgrade() {
    let mut f = Fixture::new(true);
    f.begin();
    let before = f.base.f.task.detail.usage.clone();
    let detail = f.publish().unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    let chat = chat::read_for_task(&f.base.f.store.conn, f.base.id()).unwrap();
    assert_eq!(chat.messages.len(), 2);
    assert!(matches!(
        &chat.messages[1].message,
        StoredChatMessage::ResultReference { task_id, result_id, .. }
            if task_id == f.base.id() && result_id == &f.base.f.store.investigation_result(f.base.id()).unwrap().unwrap().result_id
    ));
    assert_eq!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::InsufficientEvidence)
    );
    assert_eq!(detail.usage.tool_actions, before.tool_actions);
    let saved = f.saved();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    #[cfg(unix)]
    assert_task_load_workspace(&tx, &saved);
    let result = load_result(&tx, &saved).unwrap().unwrap();
    #[cfg(unix)]
    {
        let ledger = load_ledger(&tx, &saved).unwrap().unwrap();
        assert_eq!(
            Some(
                ledger
                    .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                    .unwrap()
            ),
            saved.ledger_hash
        );
        let (owner, _) = InvestigationInputLedger::decode_workspace_bytes().unwrap();
        assert!(
            next_prompt_ledger_workspace().unwrap()
                >= owner + coverage::archive_acquisition_workspace().unwrap()
        );
    }
    #[cfg(unix)]
    {
        // AC-0327: a real completed result is reloaded through semantic replay.
        let wire = serde_json::to_vec(&result).unwrap().len();
        let (retained, peak) = next_prompt_result_workspace().unwrap();
        let semantic = agents::investigation::completion_result_validation_workspace(
            &INVESTIGATION_V2_POLICY,
            MAX_RECORD_BYTES,
            wire,
        )
        .unwrap();
        assert!(retained >= wire && peak >= retained + semantic);
        assert!(
            agents::investigation::completion_result_validation_workspace(
                &INVESTIGATION_V2_POLICY,
                usize::MAX,
                wire,
            )
            .is_none()
        );
        assert!(
            agents::investigation::completion_result_validation_workspace(
                &INVESTIGATION_V2_POLICY,
                MAX_RECORD_BYTES,
                MAX_RECORD_BYTES + 1,
            )
            .is_none()
        );
    }
    assert_eq!(result.schema_version, 7);
    assert!(
        result
            .disclosure_hash
            .as_ref()
            .unwrap()
            .starts_with("investigation-disclosure-v6:")
    );
    assert_eq!(
        result.invocation_step_id.as_deref(),
        Some(f.response.step_id.as_str())
    );
    let mut old_request = f.base.f.request.clone();
    old_request.schema_version = 6;
    old_request.specialist_id = "domain-analyst@9".into();
    let mut old = saved.clone();
    old.schema_version = 2;
    old.detail.summary.schema_version = 6;
    old.detail.summary.specialist_id = SpecialistId::DomainAnalystV9;
    old.completion_intent = Some(host_fixture_intent(&old_request).unwrap());
    assert_eq!(
        validate_inspection_result(&old, &result),
        Err(InvestigationStoreError::Invalid)
    );
    let mut downgraded = result;
    downgraded.schema_version = 6;
    assert_eq!(
        validate_inspection_result(&saved, &downgraded),
        Err(InvestigationStoreError::Invalid)
    );
    let sql: String = tx
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='investigation_results_no_update'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    tx.execute_batch("DROP TRIGGER investigation_results_no_update")
        .unwrap();
    let body = encode(&downgraded, MAX_RESULT_BYTES).unwrap();
    tx.execute(
        "UPDATE investigation_results SET body=?1,body_hash=?2 WHERE task_id=?3",
        params![body, core_prov::content_hash(&body), f.base.id()],
    )
    .unwrap();
    tx.execute_batch(&sql).unwrap();
    tx.commit().unwrap();
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    assert!(load_next_prompt_task(&tx, f.base.id()).is_err());
}

#[test]
fn chat_result_reference_rejects_cross_task_identity_on_read() {
    let mut f = Fixture::new(true);
    f.begin();
    f.publish().unwrap();
    let id = f.base.id().to_owned();
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    let result = reopened.investigation_result(&id).unwrap().unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let message_id: String = tx
        .query_row(
            "SELECT id FROM investigation_chat_messages WHERE task_id=?1 AND ordinal=1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    let result_hash: String = tx
        .query_row(
            "SELECT body_hash FROM investigation_results WHERE task_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    let trigger: String = tx
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='investigation_chat_messages_no_update'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    tx.execute_batch("DROP TRIGGER investigation_chat_messages_no_update")
        .unwrap();
    let body = encode(
        &StoredChatMessage::ResultReference {
            task_id: "investigation:foreign-task".into(),
            result_id: result.result_id,
            result_hash,
        },
        chat::MAX_RESULT_REFERENCE_BYTES,
    )
    .unwrap();
    tx.execute(
        "UPDATE investigation_chat_messages SET body=?1,body_hash=?2 WHERE id=?3",
        params![body, core_prov::content_hash(&body), message_id],
    )
    .unwrap();
    tx.execute_batch(&trigger).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        reopened.investigation_chat(&id),
        Err(InvestigationStoreError::Invalid),
        "the app-facing reader rejects a reference to another task"
    );
    assert!(reopened.investigation_result(&id).unwrap().is_some());
}

#[test]
fn next_prompt_finish_boundary_refusals_use_existing_obligations_and_two_finish_cap() {
    // AC-0492: preserve the legacy control alongside the gated family.
    for f in [Fixture::new(false), gated_pending_finish_fixture()] {
        assert_two_finish_cap(f);
    }
}
fn assert_two_finish_cap(mut f: Fixture) {
    f.begin();
    let first = f.publish().unwrap();
    assert_eq!(first.summary.status, InvestigationStatus::Running);
    assert_eq!(first.completion.as_ref().unwrap().refusal_count, 1);
    assert_eq!(first.usage.tool_actions, 0);
    assert_eq!(f.saved().phase, Phase::Ready);
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(f.base.id()).unwrap(), first);
    f.next();
    f.begin();
    let second = f.publish().unwrap();
    assert_eq!(second.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        second.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::ObligationsUnmet)
    );
    assert_eq!(second.completion.as_ref().unwrap().refusal_count, 2);
    assert_eq!(second.usage.model_invocations, 2);
    assert_eq!(second.usage.tool_actions, 0);
    assert_eq!(second.citations.len(), 0);
    let saved = f.saved();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    #[cfg(unix)]
    assert_task_load_workspace(&tx, &saved);
    let entries = staged::load_for_task(&tx, &saved).unwrap();
    assert_eq!(entries.len(), 2);
    #[cfg(unix)]
    {
        // AC-0327 (#978): genuine two-refusal replay with retained witnesses.
        let (retained, peak) = next_prompt_staged_workspace().unwrap();
        let wire = serde_json::to_vec(&entries).unwrap().len();
        let semantic = stored_refusal_validation_workspace(
            &INVESTIGATION_V2_POLICY,
            MAX_RECORD_BYTES,
            MAX_RECORD_BYTES,
        )
        .unwrap();
        assert!(retained >= wire + std::mem::size_of_val(&entries));
        assert!(peak >= retained + semantic);
        // Corruption remains rejected by the real loader, independent of sizing.
        tx.execute_batch(
            "SAVEPOINT corrupt_feedback; DROP TRIGGER staged_completion_feedback_no_update",
        )
        .unwrap();
        tx.execute(
            "UPDATE staged_completion_feedback SET body=?1,body_hash=?2 WHERE task_id=?3 AND sequence=2",
            params![b"{}".as_slice(), core_prov::content_hash(b"{}"), f.base.id()],
        )
        .unwrap();
        assert!(staged::load_for_task(&tx, &saved).is_err());
        tx.execute_batch("ROLLBACK TO corrupt_feedback; RELEASE corrupt_feedback")
            .unwrap();
        assert_eq!(staged::load_for_task(&tx, &saved).unwrap().len(), 2);
    }
    assert_eq!(load_ledger(&tx, &saved).unwrap().unwrap(), f.base.f.ledger);
    assert_eq!(
        admission_journal::derive_current(&tx, &saved)
            .unwrap()
            .count,
        0
    );
    drop(tx);
    let before = gated_stored_rows(&f);
    assert_eq!(f.publish(), Err(InvestigationStoreError::Stale));
    assert_eq!(gated_stored_rows(&f), before);
}

#[test]
fn next_prompt_finish_boundary_late_cancel_preserves_valid_finish_and_terminates_refusal() {
    // AC-0492: identical assertions exercise both immutable families.
    for (acquired, mut f) in [
        (false, Fixture::new(false)),
        (true, Fixture::new(true)),
        (false, gated_pending_finish_fixture()),
        (true, gated_finish_fixture(false)),
    ] {
        f.begin();
        let id = f.base.id().to_owned();
        f.base
            .f
            .store
            .cancel_next_prompt_investigation(&id)
            .unwrap();
        f.base.refresh();
        let detail = f.publish().unwrap();
        assert_eq!(
            detail.summary.status,
            if acquired {
                InvestigationStatus::Completed
            } else {
                InvestigationStatus::Cancelled
            }
        );
        assert_eq!(detail.summary.has_result, acquired);
        assert_eq!(detail.usage.evidence_requests, 0);
        f.saved();
    }
}

#[test]
fn next_prompt_finish_boundary_rebinding_and_stale_proposals_leave_no_effects() {
    for kind in ["envelope", "request", "prepared", "stale", "choice_read"] {
        let mut f = Fixture::new(false);
        f.begin();
        let before = f.rows();
        if kind == "envelope" {
            f.original["outcome"] = "supported".into();
        }
        if kind == "request" {
            f.base.f.request.question.push_str(" changed");
        }
        if kind == "prepared" {
            f.prepared = Fixture::new(false).prepared;
        }
        if kind == "stale" {
            f.base.f.task.detail.summary.revision += 1;
        }
        if kind == "choice_read" {
            let query = InvestigationQuery {
                scope: InvestigationScope::All,
                kind: None,
                labels: vec![],
                max_facts: 1,
                max_bytes: 16384,
                cursor: None,
            };
            f.original = serde_json::json!({"type":"query_context","context_id":f.base.f.request.expected_context_id,"query":query});
            f.response.action_hash =
                core_prov::content_hash(&encode(&f.original, MAX_ACTION_BYTES).unwrap());
        }
        assert!(f.publish().is_err(), "{kind}");
        assert_eq!(f.rows(), before, "{kind}");
    }
}

#[test]
fn next_prompt_finish_boundary_final_write_rollback_and_lost_ack_are_exact() {
    // AC-0492: identical assertions exercise both immutable families.
    for (acquired, mut f) in [
        (false, Fixture::new(false)),
        (true, Fixture::new(true)),
        (false, gated_pending_finish_fixture()),
        (true, gated_finish_fixture(false)),
    ] {
        f.begin();
        let before = gated_stored_rows(&f);
        FAIL_BEFORE_FINISH_COMMIT.with(|flag| flag.set(true));
        assert_eq!(f.publish(), Err(InvestigationStoreError::Capacity));
        assert_eq!(gated_stored_rows(&f), before);
        assert_eq!(
            chat::read_for_task(&f.base.f.store.conn, f.base.id())
                .unwrap()
                .messages
                .len(),
            1
        );
        LOSE_FINISH_ACK.with(|flag| flag.set(true));
        assert_eq!(f.publish(), Err(InvestigationStoreError::Storage));
        let saved = f.saved();
        assert_eq!(
            saved.phase,
            if acquired {
                Phase::Terminal
            } else {
                Phase::Ready
            }
        );
        assert_eq!(saved.detail.summary.has_result, acquired);
        assert_eq!(
            chat::read_for_task(&f.base.f.store.conn, f.base.id())
                .unwrap()
                .messages
                .len(),
            if acquired { 2 } else { 1 }
        );
        let after = gated_stored_rows(&f);
        assert_eq!(f.publish(), Err(InvestigationStoreError::Stale));
        assert_eq!(gated_stored_rows(&f), after);
    }
}

// AC-0327: real pending and completed task records, including derived flatten
// and default fields. No store mutation is needed for decoder refusal controls.
#[test]
fn task_record_workspace_covers_pending_completed_and_flattened_refusal() {
    let (retained, peak) = next_prompt_record_workspace(MAX_RECORD_BYTES).unwrap();
    assert!(peak > retained);
    assert!(next_prompt_record_workspace(MAX_RECORD_BYTES + 1).is_none());
    assert!(next_prompt_record_workspace(usize::MAX).is_none());
    for completed in [false, true] {
        let mut f = Fixture::new(true);
        f.begin();
        if completed {
            f.publish().unwrap();
        }
        let task = f.saved();
        assert_eq!(task.detail.summary.has_result, completed);
        assert_eq!(task.pending.is_some(), !completed);
        let bytes = encode(&task, MAX_RECORD_BYTES).unwrap();
        let decoded: StoredTask = decode(&bytes, &core_prov::content_hash(&bytes)).unwrap();
        assert_eq!(
            serde_json::to_value(&decoded).unwrap(),
            serde_json::to_value(&task).unwrap()
        );
        let (actual_cap, actual_peak) = next_prompt_record_workspace(bytes.len()).unwrap();
        assert!(retained >= actual_cap && peak >= actual_peak);
        assert!(actual_cap >= std::mem::size_of::<StoredTask>() + bytes.len());
        let mut malformed = serde_json::to_value(&task).unwrap();
        malformed["detail"]["unexpected"] = serde_json::json!({"nested": "雪\n".repeat(1024)});
        let raw = serde_json::to_string(&malformed).unwrap();
        assert!(raw.len() < MAX_RECORD_BYTES);
        assert!(agents::investigation::decode_record::<StoredTask>(&raw).is_err());
    }
}

#[cfg(unix)]
fn assert_task_load_workspace(tx: &Connection, saved: &StoredTask) {
    // AC-0327 (#980): real full-reader replay for completed and refused tasks.
    let reopened = load_next_prompt_task(tx, &saved.detail.summary.investigation_id).unwrap();
    let actual = serde_json::to_vec(&reopened).unwrap();
    assert_eq!(actual, serde_json::to_vec(saved).unwrap());
    let (retained, peak) = next_prompt_task_load_workspace().unwrap();
    assert!(retained >= actual.len() + std::mem::size_of_val(&reopened));
    let (journal, replay) = next_prompt_staged_workspace().unwrap();
    assert!(peak >= retained + journal * 2 + replay);
}

// Cold setup only; every subsequent call/attempt is actually admitted.
fn gated_pending_finish_fixture() -> Fixture {
    use completion::next_prompt_snapshot::tests::fixture::Fixture as Acquisition;
    let base = Acquisition::customized(false, |f| {
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
    Fixture::from_base(base, false)
}
fn gated_finish_fixture(prior_refusal: bool) -> Fixture {
    let mut ready = gated_pending_finish_fixture();
    if prior_refusal {
        ready.begin();
        let original = ready.query_action();
        let mut response = ready.response.clone();
        response.action_hash =
            core_prov::content_hash(&encode(&original, MAX_ACTION_BYTES).unwrap());
        ready
            .base
            .f
            .store
            .refuse_next_prompt(
                &ready.base.f.execution,
                ready.base.f.task.detail.summary.revision,
                &response,
                &original,
                admission_journal::RefusalReason::Action(
                    agents::investigation::ActionRefusalReason::ActionNotOffered,
                ),
            )
            .unwrap();
        ready.next();
    }
    let Fixture {
        mut base,
        input,
        prepared,
        ..
    } = ready;
    base.input = input;
    base.prepared = prepared;
    base.acquire_inventory_observed(|base| {
        let tx = base.f.store.conn.unchecked_transaction().unwrap();
        let step = base.f.task.pending.as_ref().unwrap();
        assert_eq!(
            step.disclosure
                .as_ref()
                .unwrap()
                .admission
                .as_ref()
                .unwrap()
                .acquisition
                .as_ref()
                .unwrap()
                .action_feedback_hash
                .is_some(),
            prior_refusal
        );
        let basis =
            admission_journal::AuthenticatedCompletionBasis::load(&tx, &base.f.task, &step.step_id)
                .unwrap()
                .unwrap();
        assert!(
            basis
                .binding()
                .unwrap()
                .fingerprint()
                .unwrap()
                .starts_with("investigation-disclosure-v6:")
        );
    });
    Fixture::from_base(base, false)
}

#[test]
fn gated_authenticated_finish_persists_and_reopens() {
    assert_gated_finish_reopens(gated_finish_fixture(false));
}
#[test]
fn gated_authenticated_finish_reopens_prior_semantic_feedback() {
    // AC-0490: real Some feedback is authenticated before inventory clears it.
    assert_gated_finish_reopens(gated_finish_fixture(true));
}
fn assert_gated_finish_reopens(mut f: Fixture) -> Fixture {
    f.begin();
    let before = f.base.f.task.detail.usage.clone();
    let detail = f.publish().unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    assert_eq!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::InsufficientEvidence)
    );
    assert_eq!(detail.usage.tool_actions, before.tool_actions);
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(f.base.id()).unwrap(), detail);
    let result = reopened.investigation_result(f.base.id()).unwrap().unwrap();
    assert_eq!(result.schema_version, 7);
    assert_eq!(
        result.invocation_step_id.as_deref(),
        Some(f.response.step_id.as_str())
    );
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task(&tx, f.base.id()).unwrap();
    assert_eq!(load_result(&tx, &task).unwrap().unwrap(), result);
    assert!(
        task.admission_state
            .as_ref()
            .unwrap()
            .current_call
            .as_ref()
            .unwrap()
            .admission
            .acquisition
            .as_ref()
            .unwrap()
            .action_feedback_hash
            .is_none()
    );
    #[cfg(unix)]
    assert_task_load_workspace(&tx, &task);
    f
}

fn gated_stored_rows(f: &Fixture) -> Vec<(Vec<u8>, String)> {
    let connection = &f.base.f.store.conn;
    let mut rows = vec![
        connection
            .query_row(
                "SELECT record,record_hash FROM investigation_tasks WHERE id=?1",
                [f.base.id()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap(),
    ];
    for table in [
        "investigation_invocations",
        "investigation_responses",
        "investigation_results",
        "investigation_inputs",
        "investigation_events",
        "investigation_admission_refusals",
        "staged_completion_intents",
        "staged_completion_refusals",
        "staged_completion_feedback",
    ] {
        if table == "investigation_admission_refusals" || table.starts_with("staged_completion_") {
            let present: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            // Preserve namespace absence/presence as part of the snapshot too.
            rows.push((table.as_bytes().to_vec(), present.to_string()));
            if !present {
                continue;
            }
        }
        rows.extend(
            connection
                .prepare(&format!(
                    "SELECT body,body_hash FROM {table} WHERE task_id=?1 ORDER BY body,body_hash"
                ))
                .unwrap()
                .query_map([f.base.id()], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .map(Result::unwrap),
        );
    }
    rows
}

#[test]
fn gated_authenticated_finish_rejects_rehashed_stamps_without_reader_writes() {
    // AC-0490: direct factory negatives isolate authority, not a row checksum.
    for field in ["choices", "feedback"] {
        let f = assert_gated_finish_reopens(gated_finish_fixture(true));
        let mut task = f.saved();
        let id = f.base.id();
        let finishing = f.response.step_id.as_str();
        let step_id = if field == "feedback" {
            "step-2"
        } else {
            finishing
        };
        let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
        assert!(
            admission_journal::AuthenticatedCompletionBasis::load(&tx, &task, finishing)
                .unwrap()
                .is_some()
        );
        let mut step: PendingInvestigationStep =
            read_body(&tx, "investigation_invocations", "step_id", id, &step_id)
                .unwrap()
                .unwrap();
        let stamp = step
            .disclosure
            .as_mut()
            .unwrap()
            .admission
            .as_mut()
            .unwrap()
            .acquisition
            .as_mut()
            .unwrap();
        let hash = if field == "choices" {
            &mut stamp.choices_hash
        } else {
            stamp
                .action_feedback_hash
                .as_mut()
                .expect("actual prior Some")
        };
        let byte = if hash.starts_with('a') { "b" } else { "a" };
        hash.replace_range(..1, byte);
        if field == "choices" {
            task.admission_state
                .as_mut()
                .unwrap()
                .current_call
                .as_mut()
                .unwrap()
                .admission = step.disclosure.as_ref().unwrap().admission.clone().unwrap();
        }
        completion::rewrite_test_body(
            &tx,
            "investigation_invocations",
            id,
            "step_id",
            &step_id,
            &encode(&step, MAX_INVOCATION_BYTES).unwrap(),
        );
        admission_journal::tests::save_task(&tx, &task);
        assert_eq!(
            read_body::<PendingInvestigationStep>(
                &tx,
                "investigation_invocations",
                "step_id",
                id,
                &step_id
            )
            .unwrap()
            .unwrap(),
            step
        );
        if field == "choices" {
            let history = completion::SavedMetadataHistory::load(&tx, &task).unwrap();
            let call = task
                .admission_state
                .as_ref()
                .unwrap()
                .current_call
                .as_ref()
                .unwrap();
            let metadata = history.metadata_at(finishing).unwrap();
            call.validate_core(&task, &step, &f.base.f.ledger, &metadata)
                .unwrap();
            assert!(
                history
                    .acquisition_choices_at(call, &f.base.f.ledger)
                    .is_err()
            );
        }
        assert!(
            admission_journal::AuthenticatedCompletionBasis::load(&tx, &task, finishing).is_err(),
            "{field}"
        );
        tx.commit().unwrap();
        let before = gated_stored_rows(&f);
        let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
        let changes = reopened.conn.total_changes();
        assert!(reopened.investigation(id).is_err(), "{field} detail");
        assert!(reopened.investigation_result(id).is_err(), "{field} result");
        assert_eq!(reopened.conn.total_changes(), changes);
        assert_eq!(gated_stored_rows(&f), before);
    }
}

#[test]
fn gated_authenticated_finish_rejects_earlier_call_and_foreign_pair() {
    // AC-0490: the old call is valid history, but cannot bind this final result.
    let f = assert_gated_finish_reopens(gated_finish_fixture(true));
    let task = f.saved();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let basis =
        admission_journal::AuthenticatedCompletionBasis::load(&tx, &task, &f.response.step_id)
            .unwrap()
            .unwrap();
    let last: PendingInvestigationStep = read_body(
        &tx,
        "investigation_invocations",
        "step_id",
        f.base.id(),
        &f.response.step_id,
    )
    .unwrap()
    .unwrap();
    let earlier: PendingInvestigationStep = read_body(
        &tx,
        "investigation_invocations",
        "step_id",
        f.base.id(),
        &"step-2",
    )
    .unwrap()
    .unwrap();
    let old_ledger: InvestigationInputLedger =
        read_body(&tx, "investigation_inputs", "revision", f.base.id(), &0i64)
            .unwrap()
            .unwrap();
    basis.check_step(&f.base.f.ledger, &last).unwrap();
    assert!(
        admission_journal::AuthenticatedCompletionBasis::load(&tx, &task, &earlier.step_id)
            .is_err()
    );
    assert!(basis.check_step(&f.base.f.ledger, &earlier).is_err());
    assert!(basis.check_step(&old_ledger, &last).is_err());
}

// AC-0490: generic transition cannot bypass the dedicated @18 writer.
#[test]
fn gated_valid_finish_cannot_enter_generic_action_admitted() {
    let mut f = gated_finish_fixture(false);
    f.begin();
    let result = {
        let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task(&tx, f.base.id()).unwrap();
        assert_eq!(task.phase, Phase::Invocation);
        let basis =
            admission_journal::AuthenticatedCompletionBasis::load(&tx, &task, &f.response.step_id)
                .unwrap()
                .unwrap();
        let entries = attempts(&tx, &task).unwrap();
        assert_eq!(entries.len(), 1);
        let evidence = [AttemptEvidence {
            record: &entries[0].record,
            ledger: &f.base.f.ledger,
        }];
        let finish = serde_json::from_value(f.original.clone()).unwrap();
        let bound = basis.binding().unwrap();
        let result = bound
            .admit_finish(
                &f.input,
                &f.base.f.request,
                &finish,
                &evidence,
                f.response.observed_model.clone(),
                "2026-10-30T14:00:00Z".into(),
            )
            .unwrap()
            .ok()
            .unwrap();
        // Otherwise valid result through the exact dedicated host validators.
        validate_result(
            &tx,
            &task,
            &result,
            &f.base.f.ledger,
            task.pending.as_ref().unwrap(),
            Some(&basis),
        )
        .unwrap();
        result
    };
    let before = gated_stored_rows(&f);
    let usage = f.saved().detail.usage;
    let changes = f.base.f.store.conn.total_changes();
    assert_eq!(
        f.base.f.store.advance_investigation(
            &f.base.f.execution,
            f.base.f.task.detail.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: f.response.clone(),
                tool: None,
                result: Some(Box::new(result)),
            }
        ),
        Err(InvestigationStoreError::Invalid)
    );
    assert_eq!(f.base.f.store.conn.total_changes(), changes);
    assert_eq!(gated_stored_rows(&f), before);
    assert_eq!(f.saved().detail.usage, usage);
    // The same still-pending call remains usable through its dedicated writer.
    assert_eq!(
        f.publish().unwrap().summary.status,
        InvestigationStatus::Completed
    );
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    assert!(
        reopened
            .investigation_result(f.base.id())
            .unwrap()
            .is_some()
    );
}

#[test]
fn gated_finish_refusal_reopens_after_later_inventory_and_result() {
    // AC-0492: refusal remains at ledger0 even after real acquisition/finish.
    let f = historical_refusal_fixture();
    let task = f.saved();
    assert_eq!(task.detail.usage.model_invocations, 3);
    assert_eq!(task.detail.usage.tool_actions, 1);
    assert_eq!(task.detail.completion.as_ref().unwrap().refusal_count, 1);
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let entries = staged::load_for_task(&tx, &task).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].ledger_revision, 0);
    assert_eq!(entries[0].record.step_id, "step-1");
    let historical = admission_journal::AuthenticatedRefusalBasis::load(&tx, &task, 1, &entries[0])
        .unwrap()
        .unwrap();
    assert!(historical.binding().is_ok());
    assert!(admission_journal::AuthenticatedCompletionBasis::load(&tx, &task, "step-1").is_err());
}

fn historical_refusal_fixture() -> Fixture {
    let mut f = gated_pending_finish_fixture();
    f.begin();
    let first = f.publish().unwrap();
    assert_eq!(first.summary.status, InvestigationStatus::Running);
    assert_eq!(first.completion.as_ref().unwrap().refusal_count, 1);
    assert_eq!(first.usage.model_invocations, 1);
    assert_eq!(first.usage.tool_actions, 0);
    assert_eq!(f.saved().phase, Phase::Ready);
    f.next();
    let Fixture {
        mut base,
        input,
        prepared,
        ..
    } = f;
    base.input = input;
    base.prepared = prepared;
    base.acquire_inventory();
    assert_eq!(base.f.ledger.revision, 1);
    assert_gated_finish_reopens(Fixture::from_base(base, false))
}

#[test]
fn gated_historical_refusal_rejects_rehashed_disclosure_fingerprint() {
    // AC-0492: valid assessment/record identity cannot replace disclosure identity.
    let f = historical_refusal_fixture();
    let task = f.saved();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let entries = staged::load_for_task(&tx, &task).unwrap();
    let entry = &entries[0];
    let basis = admission_journal::AuthenticatedRefusalBasis::load(&tx, &task, 1, entry)
        .unwrap()
        .unwrap();
    let bound = basis.binding().unwrap();
    let contract = task.completion_intent.as_ref().unwrap().contract();
    let envelope = &contract.obligations;
    bound
        .validate_refusal(&entry.record, &task.intent_hash, envelope, &[], None)
        .unwrap();
    let mut forged = entry.record.clone();
    forged.disclosure_hash = format!("investigation-disclosure-v6:{}", "a".repeat(64));
    forged.refusal_id = forged.content_id().unwrap();
    assert!(
        bound
            .validate_refusal(&forged, &task.intent_hash, envelope, &[], None)
            .is_err()
    );
    let ledger: InvestigationInputLedger =
        read_body(&tx, "investigation_inputs", "revision", f.base.id(), &0i64)
            .unwrap()
            .unwrap();
    let step: PendingInvestigationStep = read_body(
        &tx,
        "investigation_invocations",
        "step_id",
        f.base.id(),
        &"step-1",
    )
    .unwrap()
    .unwrap();
    assert!(
        entry
            .record
            .validate_stored_basis_with_policy(
                &task.intent_hash,
                envelope,
                &ledger,
                step.disclosure.as_ref().unwrap(),
                None,
                &INVESTIGATION_V2_POLICY,
            )
            .is_err(),
        "generic validation must still reject an unbound stamp"
    );
}

#[test]
fn gated_historical_refusal_rejects_actual_row_and_response_substitution_without_writes() {
    // AC-0492: canonical body hashes are valid; actual saved association is not.
    for field in ["ledger", "response"] {
        let f = historical_refusal_fixture();
        let task = f.saved();
        let id = f.base.id();
        let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
        let mut entries = staged::load_for_task(&tx, &task).unwrap();
        let entry = &mut entries[0];
        assert!(
            admission_journal::AuthenticatedRefusalBasis::load(&tx, &task, 1, entry)
                .unwrap()
                .is_some()
        );
        let original_record = entry.record.clone();
        entry.record.response_hash = "a".repeat(64);
        entry.record.refusal_id = entry.record.content_id().unwrap();
        entry.rehash_for_test();
        assert!(
            admission_journal::AuthenticatedRefusalBasis::load(&tx, &task, 1, entry).is_err(),
            "full actual-row equality must reject a foreign record field"
        );
        entry.record = original_record;
        entry.rehash_for_test();
        if field == "ledger" {
            entry.ledger_revision = 1;
        } else {
            entry.response.step_id = f.response.step_id.clone();
        }
        // Before rewriting, the same proposed value must fail full saved-row equality.
        assert!(admission_journal::AuthenticatedRefusalBasis::load(&tx, &task, 1, entry).is_err());
        entry.rehash_for_test();
        completion::rewrite_test_body(
            &tx,
            "staged_completion_refusals",
            id,
            "sequence",
            &1u32,
            &encode(entry, MAX_RECORD_BYTES).unwrap(),
        );
        assert_eq!(
            read_body::<staged::JournalEntry>(
                &tx,
                "staged_completion_refusals",
                "sequence",
                id,
                &1u32,
            )
            .unwrap()
            .unwrap(),
            *entry
        );
        // Expected and actual now match, so this reaches the ledger/response backstop.
        assert!(
            admission_journal::AuthenticatedRefusalBasis::load(&tx, &task, 1, entry).is_err(),
            "{field}"
        );
        tx.commit().unwrap();
        let before = gated_stored_rows(&f);
        let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
        let changes = reopened.conn.total_changes();
        assert!(reopened.investigation(id).is_err(), "{field} detail");
        assert!(reopened.investigation_result(id).is_err(), "{field} result");
        assert_eq!(reopened.conn.total_changes(), changes);
        assert_eq!(gated_stored_rows(&f), before);
    }
}
