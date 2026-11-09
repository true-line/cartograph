//! Durable refusal ordering tests use actual internal prompt9/disclosure6 calls;
//! the fixture is not runtime family activation or provider/source execution.
use super::*;
use completion::{InternalFixture, internal_next_prompt_fixture};
use llm::{EgressFirewall, EgressPolicy, OllamaProvider, bounded::CompletionLimits};
use serde_json::json;
use std::time::Duration;

pub(in crate::jobs::investigations) fn proposal(f: &InternalFixture, maximum: usize) -> Value {
    let query = InvestigationQuery {
        scope: InvestigationScope::All,
        kind: None,
        labels: vec![],
        max_facts: maximum,
        max_bytes: 16384,
        cursor: None,
    };
    json!({"type":"query_context","context_id":f.request.expected_context_id,"query":query})
}
pub(in crate::jobs::investigations) fn response(
    f: &InternalFixture,
    value: &Value,
) -> InvestigationResponseMeta {
    InvestigationResponseMeta {
        finish_feedback_witness: None,
        finish_telemetry: None,
        step_id: f.step.step_id.clone(),
        response_hash: core_prov::content_hash(b"complete response"),
        action_hash: core_prov::content_hash(&encode(value, MAX_ACTION_BYTES).unwrap()),
        observed_model: None,
        reported_input_tokens: None,
        reported_output_tokens: None,
        reported_cache_read_tokens: None,
        active_milliseconds: f.task.detail.usage.active_milliseconds + 1,
    }
}
pub(in crate::jobs::investigations) fn save_task(tx: &Transaction<'_>, task: &StoredTask) {
    let body = encode(task, MAX_RECORD_BYTES).unwrap();
    tx.execute(
        "UPDATE investigation_tasks SET revision=?2,record=?3,record_hash=?4 WHERE id=?1",
        params![
            task.detail.summary.investigation_id,
            signed(task.detail.summary.revision).unwrap(),
            body,
            core_prov::content_hash(&body)
        ],
    )
    .unwrap();
}
pub(in crate::jobs::investigations) fn state(f: &InternalFixture) -> CapacityState {
    let tx = f.store.conn.unchecked_transaction().unwrap();
    let value = derive_current(&tx, &f.task).unwrap();
    tx.commit().unwrap();
    value
}
pub(in crate::jobs::investigations) fn refuse(f: &mut InternalFixture, maximum: usize) -> Entry {
    let value = proposal(f, maximum);
    let response = response(f, &value);
    let tx = f.store.conn.transaction().unwrap();
    initialize(&tx).unwrap();
    insert_body(
        &tx,
        "investigation_responses",
        &f.basis.task_id,
        &f.basis.step_id,
        &response,
        4096,
    )
    .unwrap();
    let (_, usage) = transitions::validate_response(&tx, &f.task, &response).unwrap();
    let entry = append(
        &tx,
        &f.task,
        &f.basis,
        &response,
        &value,
        CapacityRefusalReason::PromptCapacity,
    )
    .unwrap();
    f.task.detail.usage = usage;
    f.task.phase = Phase::Ready;
    f.task.pending = None;
    f.task.admission_state.as_mut().unwrap().current_call = None;
    bump(&tx, &mut f.task).unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ModelCompleted,
        Some(&response.step_id),
        None,
        "Test capacity refusal; no tool executed.",
    )
    .unwrap();
    save_task(&tx, &f.task);
    tx.commit().unwrap();
    entry
}
pub(in crate::jobs::investigations) fn start_call(
    f: &mut InternalFixture,
    pages: &[String],
    previous: Option<InvestigationInputLedger>,
) {
    let capacity = state(f);
    let policy = &INVESTIGATION_V2_POLICY;
    let turn = InvestigationTurnBasis {
        input_ledger_hash: f.ledger.fingerprint_with_policy(policy).unwrap(),
        completed_tool: previous.as_ref().map(|_| InvestigationTool::QueryContext),
        previous_ledger: previous,
    };
    let mode = f
        .metadata
        .next_call_mode_with_policy(&f.task.detail.usage, policy)
        .unwrap();
    let page_refs = pages.iter().map(String::as_str).collect::<Vec<_>>();
    let input = InvestigationInput::prepare_v9_with_policy(
        &f.request.question,
        f.task.detail.summary.specialist_id,
        f.ledger.clone(),
        &page_refs,
        &[],
        &[],
        &f.task.detail.usage,
        &turn,
        &f.request,
        &f.metadata,
        &[],
        mode,
        capacity.count,
        capacity.feedback.as_ref(),
        policy,
    )
    .unwrap();
    let input = if f
        .task
        .detail
        .summary
        .specialist_id
        .uses_acquisition_choices()
    {
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let choices = completion::SavedMetadataHistory::load(&tx, &f.task)
            .unwrap()
            .current_choices(&f.ledger, &f.metadata)
            .unwrap()
            .unwrap();
        input.with_acquisition_admission(choices, None).unwrap()
    } else {
        input
    };
    let id = format!("journal-step:{}", f.task.detail.usage.model_invocations + 1);
    let action = input.action(format!("{}:{id}", f.basis.task_id)).unwrap();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let prepared = EgressFirewall::new(EgressPolicy::default())
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    let disclosure = input
        .seal_disclosure_with_policy(&prepared, policy)
        .unwrap()
        .unwrap();
    let step = PendingInvestigationStep {
        step_id: id.clone(),
        payload_hash: prepared.preview().egress.payload_hash.clone(),
        input_ledger_hash: f.ledger.fingerprint_with_policy(policy).unwrap(),
        input_bytes: serde_json::to_vec(&action.payload).unwrap().len(),
        generated_tokens: policy.limits().generated_tokens_per_invocation,
        disclosure: Some(disclosure),
    };
    let tx = f.store.conn.transaction().unwrap();
    let call =
        SavedCallBasis::from_pre_call_saved(&tx, &f.task, &step, &f.ledger, &f.metadata).unwrap();
    insert_body(
        &tx,
        "investigation_invocations",
        &call.task_id,
        &id,
        &step,
        MAX_INVOCATION_BYTES,
    )
    .unwrap();
    f.task.phase = Phase::Invocation;
    f.task.pending = Some(step.clone());
    f.task.admission_state.as_mut().unwrap().current_call = Some(call.clone());
    f.task.detail.usage.model_invocations += 1;
    f.task.detail.usage.generated_token_reservations += step.generated_tokens;
    bump(&tx, &mut f.task).unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ModelStarted,
        Some(&id),
        None,
        "Test invocation; no provider executed.",
    )
    .unwrap();
    save_task(&tx, &f.task);
    tx.commit().unwrap();
    f.step = step;
    f.basis = call;
    let tx = f.store.conn.unchecked_transaction().unwrap();
    f.basis.validate_saved(&tx, &f.task).unwrap();
    tx.commit().unwrap();
}
pub(in crate::jobs::investigations) fn accept_query(
    f: &mut InternalFixture,
) -> (String, InvestigationInputLedger) {
    let value = proposal(f, 12);
    let response = response(f, &value);
    let before = f.ledger.clone();
    let raw=json!({"context":{"context_id":f.request.expected_context_id,"coverage":before.coverage.as_ref().unwrap().qualification,
        "response":{"snapshot_id":before.scope_snapshot_id,"view":"recovered_graph","total_selected":0,"facts":[],"next_cursor":null}},"evidence_options":[]}).to_string();
    f.ledger.revision += 1;
    f.ledger.queries.push(InvestigationQueryManifest {
        query: InvestigationQuery {
            scope: InvestigationScope::All,
            kind: None,
            labels: vec![],
            max_facts: 12,
            max_bytes: 16384,
            cursor: None,
        },
        response_hash: core_prov::content_hash(raw.as_bytes()),
        response_bytes: raw.len(),
        returned_facts: 0,
        total_selected: 0,
        has_more: false,
    });
    let tx = f.store.conn.transaction().unwrap();
    let (_, mut usage) = transitions::validate_response(&tx, &f.task, &response).unwrap();
    insert_body(
        &tx,
        "investigation_responses",
        &f.basis.task_id,
        &f.basis.step_id,
        &response,
        4096,
    )
    .unwrap();
    bump(&tx, &mut f.task).unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ModelCompleted,
        Some(&response.step_id),
        Some(InvestigationTool::QueryContext),
        "Test response.",
    )
    .unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ActionAdmitted,
        Some(&response.step_id),
        Some(InvestigationTool::QueryContext),
        "Test accepted query.",
    )
    .unwrap();
    bump(&tx, &mut f.task).unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ToolStarted,
        None,
        Some(InvestigationTool::QueryContext),
        "Test query started.",
    )
    .unwrap();
    insert_body(
        &tx,
        "investigation_inputs",
        &f.basis.task_id,
        &signed(f.ledger.revision).unwrap(),
        &f.ledger,
        MAX_MANIFEST_BYTES,
    )
    .unwrap();
    f.task.ledger_hash = Some(
        f.ledger
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap(),
    );
    f.task.ledger_revision = Some(f.ledger.revision);
    f.task.phase = Phase::Ready;
    f.task.pending = None;
    f.task.action_tool = None;
    f.task.admission_state.as_mut().unwrap().current_call = None;
    usage.tool_actions += 1;
    f.task.detail.usage = usage;
    bump(&tx, &mut f.task).unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ToolCompleted,
        None,
        Some(InvestigationTool::QueryContext),
        "Test real accepted ledger publication.",
    )
    .unwrap();
    save_task(&tx, &f.task);
    tx.commit().unwrap();
    (raw, before)
}

#[test]
fn admission_journal_refuse_accept_and_historical_cutoffs_use_logical_ordinals() {
    // AC-0287: immutable actual invocation/response/accepted input witnesses.
    let mut f = internal_next_prompt_fixture();
    let first_step = f.step.step_id.clone();
    let first = refuse(&mut f, 12);
    assert_eq!(
        state(&f),
        CapacityState {
            action_feedback: None,
            count: 1,
            feedback: first.feedback()
        }
    );
    start_call(&mut f, &[], None);
    let second_step = f.step.step_id.clone();
    let (raw, before) = accept_query(&mut f);
    assert_eq!(
        state(&f),
        CapacityState {
            action_feedback: None,
            count: 1,
            feedback: None
        }
    );
    start_call(&mut f, &[raw], Some(before));
    let third_step = f.step.step_id.clone();
    let second = refuse(&mut f, 11);
    assert_eq!(second.count, 2);
    assert_eq!(second.previous_entry_id, Some(first.entry_id.clone()));
    assert_eq!(
        state(&f),
        CapacityState {
            action_feedback: None,
            count: 2,
            feedback: second.feedback()
        }
    );
    let tx = f.store.conn.unchecked_transaction().unwrap();
    #[cfg(unix)]
    {
        // AC-0327: actual multi-call replay retains all cores and publications.
        let history = completion::SavedMetadataHistory::load(&tx, &f.task).unwrap();
        let line = timeline(&tx, &f.task, &history).unwrap();
        assert_eq!(line.calls.len(), 3);
        assert_eq!(line.publications.len(), 2);
        let (retained, peak) = next_prompt_timeline_workspace().unwrap();
        let actual: usize = line
            .calls
            .values()
            .map(|w| {
                serde_json::to_vec(&(&w.call, &w.core.step, &w.core.ledger, &w.core.metadata))
                    .unwrap()
                    .len()
                    + std::mem::size_of::<CallWitness>()
            })
            .sum();
        assert!(retained >= actual + line.publications.len() * std::mem::size_of::<Publication>());
        let (core, _) = next_prompt_saved_call_workspace().unwrap();
        assert!(retained >= core * INVESTIGATION_V2_POLICY.limits().model_invocations as usize);
        assert!(peak >= retained + next_prompt_metadata_workspace().unwrap().2);
        let entries = read_entries(&tx, &f.task, &line, &history).unwrap();
        assert_eq!(entries.len(), 2);
        let (rows, replay) = next_prompt_capacity_replay_workspace().unwrap();
        let actual = serde_json::to_vec(&entries).unwrap().len()
            + entries.len() * std::mem::size_of::<Entry>();
        assert!(rows >= actual);
        assert!(replay >= peak && replay >= retained + rows);
    }
    assert_eq!(
        derive_at(&tx, &f.task, &first_step).unwrap(),
        CapacityState::default()
    );
    assert_eq!(
        derive_at(&tx, &f.task, &second_step).unwrap(),
        CapacityState {
            action_feedback: None,
            count: 1,
            feedback: first.feedback()
        }
    );
    assert_eq!(
        derive_at(&tx, &f.task, &third_step).unwrap(),
        CapacityState {
            action_feedback: None,
            count: 1,
            feedback: None
        }
    );
    assert_eq!(
        derive_at(&tx, &f.task, "missing-step"),
        Err(InvestigationStoreError::Missing)
    );
    tx.commit().unwrap();
    let task: StoredTask =
        serde_json::from_slice(&encode(&f.task, MAX_RECORD_BYTES).unwrap()).unwrap();
    let path = f.directory.path().join("state.db");
    drop(f.store);
    let store = JobStore::open(path).unwrap();
    let tx = store.conn.unchecked_transaction().unwrap();
    assert_eq!(
        derive_current(&tx, &task).unwrap(),
        CapacityState {
            action_feedback: None,
            count: 2,
            feedback: second.feedback()
        }
    );
}

#[test]
fn admission_journal_accept_before_refusal_does_not_clear_later_feedback() {
    let mut f = internal_next_prompt_fixture();
    let (raw, before) = accept_query(&mut f);
    start_call(&mut f, &[raw], Some(before));
    let entry = refuse(&mut f, 11);
    assert_eq!(
        state(&f),
        CapacityState {
            action_feedback: None,
            count: 1,
            feedback: entry.feedback()
        }
    );
}

#[test]
fn admission_journal_requires_exact_saved_response_and_rolls_back_atomically() {
    let mut f = internal_next_prompt_fixture();
    let value = proposal(&f, 12);
    let response = response(&f, &value);
    {
        let tx = f.store.conn.transaction().unwrap();
        initialize(&tx).unwrap();
        assert_eq!(
            append(
                &tx,
                &f.task,
                &f.basis,
                &response,
                &value,
                CapacityRefusalReason::PromptCapacity
            ),
            Err(InvestigationStoreError::Missing)
        );
        insert_body(
            &tx,
            "investigation_responses",
            &f.basis.task_id,
            &f.basis.step_id,
            &response,
            4096,
        )
        .unwrap();
        let mut changed = response.clone();
        changed.response_hash = "f".repeat(64);
        assert_eq!(
            append(
                &tx,
                &f.task,
                &f.basis,
                &changed,
                &value,
                CapacityRefusalReason::PromptCapacity
            ),
            Err(InvestigationStoreError::Invalid)
        );
        let entry = append(
            &tx,
            &f.task,
            &f.basis,
            &response,
            &value,
            CapacityRefusalReason::UnprovenContribution,
        )
        .unwrap();
        assert_eq!(
            stored_bytes(&tx, Some(&f.basis.task_id)).unwrap(),
            entry.byte_charge().unwrap()
        );
        assert!(
            append(
                &tx,
                &f.task,
                &f.basis,
                &response,
                &value,
                CapacityRefusalReason::PromptCapacity
            )
            .is_err()
        );
        // Drop rolls back schema, response and refusal together.
    }
    assert_eq!(namespace_count(&f.store.conn).unwrap(), 0);
    let count: i64 = f
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM investigation_responses WHERE task_id=?1",
            [&f.basis.task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(state(&f), CapacityState::default());
}

fn rewrite_entry(tx: &Transaction<'_>, entry: &Entry) {
    tx.execute_batch("DROP TRIGGER investigation_admission_refusals_no_update")
        .unwrap();
    let body = encode(entry, MAX_ENTRY_BYTES).unwrap();
    tx.execute("UPDATE investigation_admission_refusals SET body=?2,body_hash=?3 WHERE task_id=?1 AND sequence=1",
        params![entry.call.task_id,body,core_prov::content_hash(&body)]).unwrap();
    tx.execute_batch(&schema()[1]).unwrap();
}
#[test]
fn admission_journal_reopen_rejects_rehashed_identity_chain_count_and_action_tampering() {
    let mut f = internal_next_prompt_fixture();
    let entry = refuse(&mut f, 12);
    for field in [
        "count",
        "sequence",
        "previous",
        "ordinal",
        "ledger",
        "invocation",
        "response",
        "proposal",
    ] {
        let tx = f.store.conn.transaction().unwrap();
        let mut changed = entry.clone();
        match field {
            "count" => changed.count = 0,
            "sequence" => changed.sequence = 2,
            "previous" => changed.previous_entry_id = Some("f".repeat(64)),
            "ordinal" => changed.call.invocation_ordinal = 2,
            "ledger" => changed.call.input_revision = 1,
            "invocation" => changed.invocation_hash = "f".repeat(64),
            "response" => changed.response.response_hash = "f".repeat(64),
            "proposal" => changed.proposal["query"]["max_facts"] = 11.into(),
            _ => unreachable!(),
        }
        changed.entry_id = changed.content_id().unwrap();
        rewrite_entry(&tx, &changed);
        assert!(derive_current(&tx, &f.task).is_err(), "{field}");
    }
    assert_eq!(
        state(&f),
        CapacityState {
            action_feedback: None,
            count: 1,
            feedback: entry.feedback()
        }
    );
    // A mutable task/revision counter cannot manufacture a clear publication.
    let tx = f.store.conn.unchecked_transaction().unwrap();
    let mut forged: StoredTask =
        serde_json::from_slice(&encode(&f.task, MAX_RECORD_BYTES).unwrap()).unwrap();
    forged.ledger_revision = Some(1);
    assert_eq!(
        derive_current(&tx, &forged),
        Err(InvestigationStoreError::Invalid)
    );
}

#[test]
fn admission_journal_bounds_schema_and_task_bytes_before_entry_decode() {
    let mut f = internal_next_prompt_fixture();
    refuse(&mut f, 12);
    {
        let tx = f.store.conn.transaction().unwrap();
        tx.execute_batch("CREATE TRIGGER investigation_admission_surprise AFTER INSERT ON investigation_admission_refusals BEGIN SELECT 1; END").unwrap();
        assert_eq!(
            stored_bytes(&tx, None),
            Err(InvestigationStoreError::Invalid)
        );
    }
    {
        let tx = f.store.conn.transaction().unwrap();
        tx.execute_batch("DROP TRIGGER investigation_admission_refusals_no_update")
            .unwrap();
        tx.execute(
            "UPDATE investigation_admission_refusals SET body=zeroblob(?1)",
            [MAX_ENTRY_BYTES as i64 + 1],
        )
        .unwrap();
        tx.execute_batch(&schema()[1]).unwrap();
        assert_eq!(
            stored_bytes(&tx, None),
            Err(InvestigationStoreError::Invalid)
        );
    }
    {
        let tx = f.store.conn.transaction().unwrap();
        // Bodies deliberately contain invalid JSON. Aggregate task capacity is
        // rejected by SQL preflight, before any attempt to decode those bodies.
        for sequence in 2..=64 {
            tx.execute(
                "INSERT INTO investigation_admission_refusals VALUES (?1,?2,?3,?2,zeroblob(?4),?5)",
                params![
                    f.basis.task_id,
                    sequence,
                    format!("test-step-{sequence}"),
                    MAX_ENTRY_BYTES as i64,
                    "f".repeat(64)
                ],
            )
            .unwrap();
        }
        assert_eq!(
            stored_bytes(&tx, Some(&f.basis.task_id)),
            Err(InvestigationStoreError::Invalid)
        );
    }
    assert_eq!(state(&f).count, 1);
}

#[test]
fn admission_journal_keeps_absent_namespace_and_existing_task_bytes_unchanged() {
    let mut f = internal_next_prompt_fixture();
    let before: Vec<u8> = f
        .store
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [&f.basis.task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_bytes(&f.store.conn, None).unwrap(), 0);
    assert_eq!(validate_optional_schema(&f.store.conn).unwrap(), None);
    let tx = f.store.conn.transaction().unwrap();
    initialize(&tx).unwrap();
    assert_eq!(stored_bytes(&tx, None).unwrap(), 0);
    let after: Vec<u8> = tx
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [&f.basis.task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before, after);
    tx.commit().unwrap();
    assert_eq!(
        stored_bytes(&f.store.conn, None),
        Err(InvestigationStoreError::Invalid)
    );
}

#[test]
fn admission_journal_capacity_class_never_accepts_finish_or_finish_only_tool() {
    let f = internal_next_prompt_fixture();
    let finish = StagedFinish {
        narrative: None,
        schema_version: 1,
        outcome: CompletionOutcome::InsufficientEvidence,
        action: InvestigationAction::Finish {
            findings: vec![],
            knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
            limitations: vec!["No evidence supports a conclusion.".into()],
        },
    };
    let value = serde_json::to_value(&finish).unwrap();
    let finish_response = response(&f, &value);
    assert!(
        StagedFinish::decode(
            std::str::from_utf8(&encode(&value, MAX_ACTION_BYTES).unwrap()).unwrap()
        )
        .is_ok()
    );
    assert!(matches!(
        validate_proposal(
            &value,
            &finish_response,
            &f.ledger,
            &f.basis,
            f.task.detail.summary.specialist_id,
            &INVESTIGATION_V2_POLICY,
        ),
        Err(InvestigationStoreError::Invalid)
    ));
    let value = proposal(&f, 12);
    let response = response(&f, &value);
    let mut call = f.basis.clone();
    call.admission.mode = CompletionCallMode::FinishOnly;
    assert!(matches!(
        validate_proposal(
            &value,
            &response,
            &f.ledger,
            &call,
            f.task.detail.summary.specialist_id,
            &INVESTIGATION_V2_POLICY,
        ),
        Err(InvestigationStoreError::Invalid)
    ));
}

#[test]
fn admission_journal_missing_or_rebound_accepted_response_never_clears_feedback() {
    let mut f = internal_next_prompt_fixture();
    refuse(&mut f, 12);
    start_call(&mut f, &[], None);
    accept_query(&mut f);
    assert!(state(&f).feedback.is_none());
    for mutation in ["missing", "action_hash"] {
        let tx = f.store.conn.transaction().unwrap();
        let trigger_name = if mutation == "missing" {
            "investigation_responses_no_delete"
        } else {
            "investigation_responses_no_update"
        };
        let sql: String = tx
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name=?1",
                [trigger_name],
                |row| row.get(0),
            )
            .unwrap();
        tx.execute_batch(&format!("DROP TRIGGER {trigger_name}"))
            .unwrap();
        if mutation == "missing" {
            tx.execute(
                "DELETE FROM investigation_responses WHERE task_id=?1 AND step_id=?2",
                params![f.basis.task_id, f.basis.step_id],
            )
            .unwrap();
        } else {
            let mut response: InvestigationResponseMeta = read_body(
                &tx,
                "investigation_responses",
                "step_id",
                &f.basis.task_id,
                &f.basis.step_id,
            )
            .unwrap()
            .unwrap();
            response.action_hash = "f".repeat(64);
            let body = encode(&response, 4096).unwrap();
            tx.execute("UPDATE investigation_responses SET body=?3,body_hash=?4 WHERE task_id=?1 AND step_id=?2",
                params![f.basis.task_id,f.basis.step_id,body,core_prov::content_hash(&body)]).unwrap();
        }
        tx.execute_batch(&sql).unwrap();
        assert!(derive_current(&tx, &f.task).is_err(), "{mutation}");
    }
}

#[test]
fn admission_journal_saved_call_reopen_authenticates_nonzero_feedback_and_count() {
    let mut f = internal_next_prompt_fixture();
    let entry = refuse(&mut f, 12);
    start_call(&mut f, &[], None);
    {
        let tx = f.store.conn.transaction().unwrap();
        f.basis.validate_saved(&tx, &f.task).unwrap();
        // Rehashing a structurally valid but different refusal cannot change
        // the feedback originally sealed into this actual saved invocation.
        let mut changed = entry.clone();
        changed.reason = RefusalReason::Capacity(CapacityRefusalReason::UnprovenContribution);
        changed.entry_id = changed.content_id().unwrap();
        rewrite_entry(&tx, &changed);
        assert!(f.basis.validate_saved(&tx, &f.task).is_err());
    }
    let task: StoredTask =
        serde_json::from_slice(&encode(&f.task, MAX_RECORD_BYTES).unwrap()).unwrap();
    let basis = f.basis.clone();
    let path = f.directory.path().join("state.db");
    drop(f.store);
    let store = JobStore::open(path).unwrap();
    let tx = store.conn.unchecked_transaction().unwrap();
    basis.validate_saved(&tx, &task).unwrap();
    #[cfg(unix)]
    {
        // AC-0327: the real reopened call retains its step across refusal replay.
        let core = admission_contract::load_saved_call_basis_core(&tx, &task, &basis).unwrap();
        let step_bytes = serde_json::to_vec(&core.step).unwrap().len();
        assert!(
            next_prompt_saved_validation_workspace().unwrap().0
                >= step_bytes + next_prompt_capacity_replay_workspace().unwrap().1
        );
    }
    let mut changed = basis.clone();
    changed.admission.capacity_refusal_count = 0;
    changed.admission.capacity_feedback = None;
    assert!(changed.validate_saved(&tx, &task).is_err());
    let mut changed = basis;
    changed
        .admission
        .capacity_feedback
        .as_mut()
        .unwrap()
        .action_hash = "f".repeat(64);
    assert!(changed.validate_saved(&tx, &task).is_err());
}

// AC-0461: a structurally valid new query does not widen a retained old task.
#[test]
fn exact_node_old_saved_ledger_is_rejected_by_persisted_role() {
    let f = internal_next_prompt_fixture();
    let mut ledger = f.ledger.clone();
    ledger.revision += 1;
    ledger.queries.push(InvestigationQueryManifest {
        query: InvestigationQuery {
            scope: InvestigationScope::Node {
                id: "caller-node".into(),
            },
            kind: Some(InvestigationFactKind::Node),
            labels: vec![],
            max_facts: 1,
            max_bytes: 32768,
            cursor: None,
        },
        response_hash: core_prov::content_hash(b"{}"),
        response_bytes: 2,
        returned_facts: 0,
        total_selected: 0,
        has_more: false,
    });
    ledger
        .validate_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let tx = f.store.conn.unchecked_transaction().unwrap();
    insert_body(
        &tx,
        "investigation_inputs",
        &f.basis.task_id,
        &signed(ledger.revision).unwrap(),
        &ledger,
        MAX_MANIFEST_BYTES,
    )
    .unwrap();
    let mut task: StoredTask =
        serde_json::from_slice(&encode(&f.task, MAX_RECORD_BYTES).unwrap()).unwrap();
    task.ledger_revision = Some(ledger.revision);
    task.ledger_hash = Some(
        ledger
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap(),
    );
    assert!(storage::load_ledger(&tx, &task).is_err());
    task.detail.summary.specialist_id = SpecialistId::DomainAnalystV17;
    assert_eq!(storage::load_ledger(&tx, &task).unwrap(), Some(ledger));
}

#[test]
fn admission_journal_capacity_only_preserves_legacy_bytes_hashes_and_counts() {
    // AC-0482: retain the pre-semantic entry layout and v1 hash domain exactly.
    #[derive(Serialize)]
    struct LegacyCapacityEntry<'a> {
        schema_version: u32,
        entry_id: &'a str,
        sequence: u32,
        count: u32,
        call: &'a SavedCallBasis,
        invocation_hash: &'a str,
        response: &'a InvestigationResponseMeta,
        proposal: &'a Value,
        reason: CapacityRefusalReason,
        previous_entry_id: &'a Option<String>,
    }
    let mut f = internal_next_prompt_fixture();
    let first = refuse(&mut f, 12);
    start_call(&mut f, &[], None);
    let second = refuse(&mut f, 13);
    assert_eq!(
        second.previous_entry_id.as_deref(),
        Some(first.entry_id.as_str())
    );
    for (index, entry) in [first, second].iter().enumerate() {
        assert_eq!(
            (entry.sequence, entry.count),
            (index as u32 + 1, index as u32 + 1)
        );
        let mut legacy = LegacyCapacityEntry {
            schema_version: 1,
            entry_id: &entry.entry_id,
            sequence: entry.sequence,
            count: entry.count,
            call: &entry.call,
            invocation_hash: &entry.invocation_hash,
            response: &entry.response,
            proposal: &entry.proposal,
            reason: CapacityRefusalReason::PromptCapacity,
            previous_entry_id: &entry.previous_entry_id,
        };
        let bytes = encode(&legacy, MAX_ENTRY_BYTES).unwrap();
        assert_eq!(encode(entry, MAX_ENTRY_BYTES).unwrap(), bytes);
        let decoded: Entry = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(&decoded, entry);
        legacy.entry_id = "";
        let mut framed = b"cartograph:investigation-admission-refusal:v1\0".to_vec();
        framed.extend(encode(&legacy, MAX_ENTRY_BYTES).unwrap());
        assert_eq!(entry.entry_id, core_prov::content_hash(&framed));
        assert_eq!(entry.content_id().unwrap(), entry.entry_id);
    }
    assert_eq!(state(&f).count, 2);
    assert!(state(&f).action_feedback.is_none());
}

// AC-0266: a refused query consumes its admitted tool but creates no input.
#[test]
fn next_prompt_query_refusal_replay_consumes_attempt_without_publication() {
    use agents::investigation::query_refusal::{QueryRefusalRecord, QueryRejection};
    let mut f = internal_next_prompt_fixture();
    let mut value = proposal(&f, 1);
    value["query"]["scope"] = json!({"type":"neighborhood","anchor":"absent","hops":1});
    let response = response(&f, &value);
    let mut record = QueryRefusalRecord {
        schema_version: 1,
        record_id: String::new(),
        task_id: f.basis.task_id.clone(),
        step_id: f.basis.step_id.clone(),
        action_hash: response.action_hash.clone(),
        input_ledger_hash: f
            .ledger
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap(),
        input_revision: f.ledger.revision,
        context_id: Some(f.request.expected_context_id.clone()),
        query: serde_json::from_value(value["query"].clone()).unwrap(),
        reason: QueryRejection::UnknownAnchor,
    };
    record.record_id = record.content_id().unwrap();
    let tx = f.store.conn.transaction().unwrap();
    let (_, mut usage) = transitions::validate_response(&tx, &f.task, &response).unwrap();
    usage.tool_actions += 1;
    f.task.detail.usage = usage;
    insert_body(
        &tx,
        "investigation_responses",
        &f.basis.task_id,
        &f.basis.step_id,
        &response,
        4096,
    )
    .unwrap();
    bump(&tx, &mut f.task).unwrap();
    for kind in [
        InvestigationEventKind::ModelCompleted,
        InvestigationEventKind::ActionAdmitted,
        InvestigationEventKind::ToolStarted,
    ] {
        event(
            &tx,
            &mut f.task,
            kind,
            Some(&f.basis.step_id),
            Some(InvestigationTool::QueryContext),
            "Refusal replay fixture.",
        )
        .unwrap();
    }
    f.task.phase = Phase::Tool;
    f.task.action_tool = Some(InvestigationTool::QueryContext);
    super::super::query_refusal::publish(&tx, &mut f.task, record).unwrap();
    f.task.pending = None;
    f.task.admission_state.as_mut().unwrap().current_call = None;
    save_task(&tx, &f.task);
    let history = completion::SavedMetadataHistory::load(&tx, &f.task).unwrap();
    let line = timeline(&tx, &f.task, &history).unwrap();
    assert_eq!(line.calls.len(), 1);
    assert_eq!(line.publications.len(), 1); // Initial context only.
    assert_eq!(line.publications[0].ordinal, 0);
    assert_eq!(f.task.ledger_revision, Some(f.ledger.revision));
    derive_current(&tx, &f.task).unwrap();
    tx.commit().unwrap();
}

#[test]
fn frozen_request_timeout_preserves_historical_bytes_and_rejects_invalid_origins() {
    let mut f = internal_next_prompt_fixture();
    let mut intent = f.task.completion_intent.clone().unwrap();
    let original = serde_json::to_vec(&intent).unwrap();
    assert!(
        serde_json::to_value(&intent)
            .unwrap()
            .get("request_timeout_ms")
            .is_none()
    );
    assert_eq!(intent.effective_request_timeout_ms(), 180_000);
    intent.validate(&f.task).unwrap();
    for timeout in [180_000, 180_001, 600_000] {
        intent.active_seconds = Some(f.task.detail.limits.active_seconds);
        intent.request_timeout_ms = Some(timeout);
        intent.validate(&f.task).unwrap();
        let saved = serde_json::to_vec(&intent).unwrap();
        let reopened: completion::CompletionIntent = serde_json::from_slice(&saved).unwrap();
        assert_eq!(reopened.effective_request_timeout_ms(), timeout);
        reopened.validate(&f.task).unwrap();
        f.task.detail.origin = InvestigationOrigin::Mcp {
            client_id: McpClientId::from_host_bytes([8; 16]),
        };
        assert!(reopened.validate(&f.task).is_err());
        f.task.detail.origin = InvestigationOrigin::App;
    }
    for timeout in [0, 179_999, 600_001, u64::MAX] {
        intent.active_seconds = Some(f.task.detail.limits.active_seconds);
        intent.request_timeout_ms = Some(timeout);
        assert!(intent.validate(&f.task).is_err());
    }
    intent.request_timeout_ms = None;
    intent.active_seconds = None;
    assert_eq!(serde_json::to_vec(&intent).unwrap(), original);
    f.task.detail.limits.active_seconds = 3600;
    assert!(intent.validate(&f.task).is_err());
    intent.active_seconds = Some(3600);
    assert!(intent.validate(&f.task).is_err());
    intent.request_timeout_ms = Some(600_000);
    intent.validate(&f.task).unwrap();
}

#[test]
fn configured_desktop_start_freezes_time_limits_and_nonce_retry_keeps_them() {
    let mut f = internal_next_prompt_fixture();
    let mut request = f.request.clone();
    request.specialist_id = "domain-analyst@18".into();
    request.request_nonce = "configured-time-start".into();
    let provider = f.task.detail.provider.clone();
    let started = f
        .store
        .start_configured_next_prompt_investigation(&request, Some(&provider), 3600, 600_000)
        .unwrap();
    assert!(started.created);
    assert_eq!(started.detail.limits.active_seconds, 3600);
    assert_eq!(started.detail.limits.wall_seconds, 3600);
    let id = &started.detail.summary.investigation_id;
    let tx = f.store.conn.unchecked_transaction().unwrap();
    let task = load_execution_task(&tx, id).unwrap();
    let intent = task.completion_intent.as_ref().unwrap();
    assert_eq!(intent.active_seconds, Some(3600));
    assert_eq!(intent.effective_request_timeout_ms(), 600_000);
    tx.commit().unwrap();
    let retried = f
        .store
        .start_configured_next_prompt_investigation(&request, Some(&provider), 600, 180_000)
        .unwrap();
    assert!(!retried.created);
    assert_eq!(retried.detail, started.detail);
    assert_eq!(
        f.store.existing_investigation_completion(&request).unwrap(),
        Some(started.detail)
    );
    request.request_nonce = "invalid-time-start".into();
    for (active, timeout) in [
        (599, 180_000),
        (3601, 180_000),
        (600, 179_999),
        (600, 600_001),
    ] {
        assert!(
            f.store
                .start_configured_next_prompt_investigation(
                    &request,
                    Some(&provider),
                    active,
                    timeout,
                )
                .is_err()
        );
        assert!(
            f.store
                .existing_investigation_completion(&request)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn capacity_exclusion_resolves_replay_query_without_rewriting_journal_hash() {
    let mut f = internal_next_prompt_fixture();
    let (page, previous) = accept_query(&mut f);
    start_call(&mut f, &[page], Some(previous));
    let reference = QueryHistoryReference::from_ledger(
        &f.basis.request_hash,
        &f.ledger,
        0,
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    let direct = proposal(&f, 12);
    let replay = json!({"type":"replay_query", "context_id":f.request.expected_context_id,
        "query_index":reference.query_index,"query_id":reference.query_id});
    let mut entry = refuse(&mut f, 12);
    entry.proposal = replay.clone();
    entry.response.action_hash =
        core_prov::content_hash(&encode(&replay, MAX_ACTION_BYTES).unwrap());
    let original_hash = entry.response.action_hash.clone();
    let role = SpecialistId::DomainAnalystV21;
    let expected = core_prov::content_hash(&encode(&direct, MAX_ACTION_BYTES).unwrap());
    assert_ne!(original_hash, expected);
    assert_eq!(
        capacity_exclusion_hash(&entry, &f.ledger, role, &INVESTIGATION_V2_POLICY).unwrap(),
        expected
    );
    assert_eq!(entry.response.action_hash, original_hash);
    // A different request binding, invalid reference, or changed wire hash
    // must not acquire exclusion authority from an otherwise valid ledger.
    for mutation in ["request", "reference", "hash"] {
        let mut bad = entry.clone();
        match mutation {
            "request" => bad.call.request_hash = "f".repeat(64),
            "reference" => bad.proposal["query_id"] = json!("f".repeat(64)),
            _ => bad.response.action_hash = "f".repeat(64),
        }
        assert!(
            capacity_exclusion_hash(&bad, &f.ledger, role, &INVESTIGATION_V2_POLICY).is_err(),
            "{mutation}"
        );
    }
}
