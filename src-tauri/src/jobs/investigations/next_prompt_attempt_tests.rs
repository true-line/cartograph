//! Exact-step acquisition publication regressions (no provider or source IO).
use super::*;

struct Publication {
    task: StoredTask,
    ledger: InvestigationInputLedger,
    response: InvestigationResponseMeta,
    read: ReadIntent,
    outcome: AttemptOutcome,
    records: Vec<AttemptRecord>,
}

fn publication(tx: &Transaction<'_>, f: &InternalFixture) -> Publication {
    let (context_id, repo, inventory_hash) = f
        .request
        .obligations
        .obligations
        .iter()
        .find_map(|obligation| {
            if let Obligation::Inventory { target, .. } = obligation {
                Some((
                    target.context_id.clone(),
                    target.repo.clone(),
                    target.inventory_hash.clone(),
                ))
            } else {
                None
            }
        })
        .unwrap();
    let request = InvestigationCoverageRead {
        context_id,
        repo,
        inventory_hash,
        max_bytes: MAX_QUERY_BYTES,
    };
    let archive = coverage::load(tx, &f.basis.task_id).unwrap().unwrap();
    let raw = encode(&archive.read(&request).unwrap(), request.max_bytes).unwrap();
    let mut ledger = f.ledger.clone();
    ledger.revision += 1;
    let index = ledger.coverage_reads.len();
    ledger
        .coverage_reads
        .push(InvestigationCoverageReadManifest {
            request: request.clone(),
            response_hash: core_prov::content_hash(&raw),
            response_bytes: raw.len(),
        });
    let read = ReadIntent::Inventory {
        context_id: request.context_id,
        repo: request.repo,
        inventory_hash: request.inventory_hash,
        max_bytes: request.max_bytes,
    };
    let response = InvestigationResponseMeta {
        step_id: f.step.step_id.clone(),
        response_hash: core_prov::content_hash(b"complete inventory action"),
        action_hash: staged::read_action_hash(&read).unwrap(),
        observed_model: None,
        reported_input_tokens: None,
        reported_output_tokens: None,
        reported_cache_read_tokens: None,
        active_milliseconds: f.task.detail.usage.active_milliseconds + 1,
    };
    let (_, mut usage) = transitions::validate_response(tx, &f.task, &response).unwrap();
    let mut task: StoredTask =
        serde_json::from_slice(&encode(&f.task, MAX_RECORD_BYTES).unwrap()).unwrap();
    insert_body(
        tx,
        "investigation_responses",
        &f.basis.task_id,
        &f.step.step_id,
        &response,
        4096,
    )
    .unwrap();
    bump(tx, &mut task).unwrap();
    for kind in [
        InvestigationEventKind::ModelCompleted,
        InvestigationEventKind::ActionAdmitted,
        InvestigationEventKind::ToolStarted,
    ] {
        event(
            tx,
            &mut task,
            kind,
            Some(&f.step.step_id),
            Some(InvestigationTool::ReadCoverageInventory),
            "Atomic metadata publication test.",
        )
        .unwrap();
    }
    transitions::persist_input_rows(tx, &task, &ledger).unwrap();
    usage.tool_actions += 1;
    task.detail.usage = usage;
    task.ledger_hash = Some(
        ledger
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap(),
    );
    task.ledger_revision = Some(ledger.revision);
    task.pending = None;
    task.approved = false;
    task.action_tool = None;
    task.phase = Phase::Ready;
    task.admission_state.as_mut().unwrap().current_call = None;
    bump(tx, &mut task).unwrap();
    event(
        tx,
        &mut task,
        InvestigationEventKind::ToolCompleted,
        Some(&f.step.step_id),
        Some(InvestigationTool::ReadCoverageInventory),
        "Exact retained inventory acquisition completed.",
    )
    .unwrap();
    let outcome = AttemptOutcome::InventoryAcquired { read_index: index };
    let records = derive_attempt_records(
        &task,
        &f.step.step_id,
        &response,
        &read,
        &outcome,
        Some(&ledger),
        0,
    )
    .unwrap();
    Publication {
        task,
        ledger,
        response,
        read,
        outcome,
        records,
    }
}

fn append(
    tx: &Transaction<'_>,
    f: &InternalFixture,
    p: &Publication,
) -> Result<(), InvestigationStoreError> {
    append_next_prompt_attempt_at_step(
        tx,
        &p.task,
        &f.basis,
        &p.response,
        &p.read,
        &p.outcome,
        &p.ledger,
        &p.records,
    )
}

#[test]
fn next_prompt_attempt_exact_saved_call_suffix_reopens_and_rejects_duplicate() {
    // AC-0287: explicit logical invocation; expected projection is rederived.
    let f = internal_next_prompt_fixture();
    let tx = f.store.conn.unchecked_transaction().unwrap();
    let p = publication(&tx, &f);
    append(&tx, &f, &p).unwrap();
    let saved = attempts(&tx, &p.task).unwrap();
    assert_eq!(
        saved
            .iter()
            .map(|entry| entry.record.clone())
            .collect::<Vec<_>>(),
        p.records
    );
    assert_eq!(append(&tx, &f, &p), Err(InvestigationStoreError::Conflict));
    storage::save_next_prompt_task(&tx, &p.task, f.task.detail.summary.revision, false).unwrap();
    tx.commit().unwrap();
    let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = storage::load_next_prompt_task_base(&tx, &f.basis.task_id).unwrap();
    assert_eq!(attempts(&tx, &task).unwrap().len(), 1);
    assert_eq!(
        metadata_current_next_prompt(&tx, &task)
            .unwrap()
            .completed_attempt_ids,
        p.records
            .iter()
            .map(|record| record.attempt_id.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn next_prompt_attempt_projection_response_and_event_rebinding_fail_before_append() {
    for mutation in ["record", "response", "ordinal", "event", "outcome"] {
        let f = internal_next_prompt_fixture();
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let mut p = publication(&tx, &f);
        let mut basis = f.basis.clone();
        match mutation {
            "record" => p.records[0].attempt_id = "a".repeat(64),
            "response" => p.response.reported_input_tokens = Some(999),
            "ordinal" => basis.invocation_ordinal += 1,
            "event" => p.task.detail.summary.last_event_sequence -= 1,
            "outcome" => p.outcome = AttemptOutcome::InventoryAcquired { read_index: 1 },
            _ => unreachable!(),
        }
        assert!(
            append_next_prompt_attempt_at_step(
                &tx,
                &p.task,
                &basis,
                &p.response,
                &p.read,
                &p.outcome,
                &p.ledger,
                &p.records
            )
            .is_err(),
            "{mutation}"
        );
        let count: i64 = tx
            .query_row(
                "SELECT count(*) FROM completion_attempts WHERE task_id=?1",
                [&f.basis.task_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "{mutation}");
    }
}

#[test]
fn next_prompt_attempt_caller_rollback_removes_response_ledger_events_and_suffix() {
    let f = internal_next_prompt_fixture();
    let events = f.task.detail.summary.last_event_sequence;
    {
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let p = publication(&tx, &f);
        append(&tx, &f, &p).unwrap();
        assert_eq!(attempts(&tx, &p.task).unwrap().len(), 1);
        // No commit: simulate a later task-capacity/CAS/publication failure.
    }
    let tx = f.store.conn.unchecked_transaction().unwrap();
    for table in ["completion_attempts", "investigation_responses"] {
        let count: i64 = tx
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE task_id=?1"),
                [&f.basis.task_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM investigation_inputs WHERE task_id=?1",
            [&f.basis.task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM investigation_events WHERE task_id=?1 AND sequence>?2",
            params![f.basis.task_id, signed(events).unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
