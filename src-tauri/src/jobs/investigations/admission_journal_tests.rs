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
        step_id: f.step.step_id.clone(),
        response_hash: core_prov::content_hash(b"complete response"),
        action_hash: core_prov::content_hash(&encode(value, MAX_ACTION_BYTES).unwrap()),
        observed_model: None,
        reported_input_tokens: None,
        reported_output_tokens: None,
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
    let id = format!("journal-step:{}", f.task.detail.usage.model_invocations + 1);
    let action = input.action(id.clone()).unwrap();
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
            count: 1,
            feedback: Some(first.feedback())
        }
    );
    start_call(&mut f, &[], None);
    let second_step = f.step.step_id.clone();
    let (raw, before) = accept_query(&mut f);
    assert_eq!(
        state(&f),
        CapacityState {
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
            count: 2,
            feedback: Some(second.feedback())
        }
    );
    let tx = f.store.conn.unchecked_transaction().unwrap();
    assert_eq!(
        derive_at(&tx, &f.task, &first_step).unwrap(),
        CapacityState::default()
    );
    assert_eq!(
        derive_at(&tx, &f.task, &second_step).unwrap(),
        CapacityState {
            count: 1,
            feedback: Some(first.feedback())
        }
    );
    assert_eq!(
        derive_at(&tx, &f.task, &third_step).unwrap(),
        CapacityState {
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
            count: 2,
            feedback: Some(second.feedback())
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
            count: 1,
            feedback: Some(entry.feedback())
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
            count: 1,
            feedback: Some(entry.feedback())
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
    assert_eq!(
        validate_proposal(&value, &finish_response, &f.ledger, &f.basis),
        Err(InvestigationStoreError::Invalid)
    );
    let value = proposal(&f, 12);
    let response = response(&f, &value);
    let mut call = f.basis.clone();
    call.admission.mode = CompletionCallMode::FinishOnly;
    assert_eq!(
        validate_proposal(&value, &response, &f.ledger, &call),
        Err(InvestigationStoreError::Invalid)
    );
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
        changed.reason = CapacityRefusalReason::UnprovenContribution;
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
