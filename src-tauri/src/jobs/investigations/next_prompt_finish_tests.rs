//! AC-0287: actual complete-response decisions from genuine production starts.
use super::*;
use crate::jobs::investigations::chat::StoredChatMessage;
#[path = "next_prompt_finish_fixture.rs"]
pub(super) mod fixture;
use fixture::Fixture;

#[path = "next_prompt_finish_only_tests.rs"]
mod finish_only;

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
    let mut f = Fixture::new(false);
    f.begin();
    let first = f.publish().unwrap();
    assert_eq!(first.summary.status, InvestigationStatus::Running);
    assert_eq!(first.completion.as_ref().unwrap().refusal_count, 1);
    assert_eq!(first.usage.tool_actions, 0);
    assert_eq!(f.saved().phase, Phase::Ready);
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
}

#[test]
fn next_prompt_finish_boundary_late_cancel_preserves_valid_finish_and_terminates_refusal() {
    for acquired in [false, true] {
        let mut f = Fixture::new(acquired);
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
    for acquired in [false, true] {
        let mut f = Fixture::new(acquired);
        f.begin();
        let before = f.rows();
        FAIL_BEFORE_FINISH_COMMIT.with(|flag| flag.set(true));
        assert_eq!(f.publish(), Err(InvestigationStoreError::Capacity));
        assert_eq!(f.rows(), before);
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
        let after = f.rows();
        assert_eq!(f.publish(), Err(InvestigationStoreError::Stale));
        assert_eq!(f.rows(), after);
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
