use super::super::{admission_journal::tests as fixture, completion::internal_next_prompt_fixture};
use super::*;

#[test]
fn capacity_transition_rejects_scalar_corruption_without_reviving_task() {
    // AC-0287: the saved row, not just its JSON, is the admission authority.
    for corrupt in [
        "UPDATE investigation_tasks SET live=0",
        "UPDATE investigation_tasks SET job_id=job_id+1",
        "UPDATE investigation_tasks SET nonce='different-nonce'",
        "UPDATE investigation_tasks SET conversation_id='different-conversation'",
        "UPDATE investigation_tasks SET record_hash=printf('%0100000d',0)",
    ] {
        let mut f = internal_next_prompt_fixture();
        let proposal = fixture::proposal(&f, 1);
        let response = fixture::response(&f, &proposal);
        f.store.conn.execute_batch(corrupt).unwrap();
        let before_events: i64 = f
            .store
            .conn
            .query_row("SELECT count(*) FROM investigation_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        {
            let tx = f.store.conn.transaction().unwrap();
            assert!(
                refuse_capacity(
                    &tx,
                    &f.task,
                    &response,
                    &proposal,
                    CapacityRefusalReason::PromptCapacity
                )
                .is_err(),
                "{corrupt}"
            );
            let responses: i64 = tx
                .query_row("SELECT count(*) FROM investigation_responses", [], |row| {
                    row.get(0)
                })
                .unwrap();
            let events: i64 = tx
                .query_row("SELECT count(*) FROM investigation_events", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(responses, 0, "{corrupt}");
            assert_eq!(events, before_events, "{corrupt}");
        }
    }
}

#[test]
fn capacity_transition_persists_response_feedback_and_ready_without_acquisition() {
    let mut f = internal_next_prompt_fixture();
    let proposal = fixture::proposal(&f, 1);
    let mut response = fixture::response(&f, &proposal);
    response.reported_input_tokens = Some(10);
    response.reported_output_tokens = Some(5);
    let before_usage = f.task.detail.usage.clone();
    let before_ledger = encode(&f.ledger, MAX_MANIFEST_BYTES).unwrap();
    let tx = f.store.conn.transaction().unwrap();
    let next = refuse_capacity(
        &tx,
        &f.task,
        &response,
        &proposal,
        CapacityRefusalReason::PromptCapacity,
    )
    .unwrap();
    assert_eq!(next.phase, Phase::Ready);
    assert!(next.pending.is_none());
    assert!(
        next.admission_state
            .as_ref()
            .unwrap()
            .current_call
            .is_none()
    );
    assert_eq!(
        next.detail.usage.model_invocations,
        before_usage.model_invocations
    );
    assert_eq!(
        next.detail.usage.generated_token_reservations,
        before_usage.generated_token_reservations
    );
    assert_eq!(next.detail.usage.tool_actions, before_usage.tool_actions);
    assert_eq!(next.detail.usage.reported_input_tokens, Some(10));
    assert_eq!(next.detail.usage.reported_output_tokens, Some(5));
    assert_eq!(
        next.detail.usage.active_milliseconds,
        response.active_milliseconds
    );
    assert_eq!(
        next.detail.usage.evidence_requests,
        before_usage.evidence_requests
    );
    assert_eq!(
        next.detail.usage.evidence_items,
        before_usage.evidence_items
    );
    let state = admission_journal::derive_current(&tx, &next).unwrap();
    assert_eq!(state.count, 1);
    assert_eq!(state.feedback.unwrap().action_hash, response.action_hash);
    let ledger: InvestigationInputLedger = read_body(
        &tx,
        "investigation_inputs",
        "revision",
        &f.basis.task_id,
        &0i64,
    )
    .unwrap()
    .unwrap();
    assert_eq!(encode(&ledger, MAX_MANIFEST_BYTES).unwrap(), before_ledger);
    tx.commit().unwrap();
    {
        let tx = f.store.conn.transaction().unwrap();
        assert!(
            refuse_capacity(
                &tx,
                &f.task,
                &response,
                &proposal,
                CapacityRefusalReason::PromptCapacity
            )
            .is_err()
        );
        // Retrying with the unacknowledged old record cannot double-charge.
    }
    f.task = next;
    assert_eq!(fixture::state(&f).count, 1);
}

#[test]
fn capacity_transition_failed_proposal_rolls_back_response_and_keeps_caller_state() {
    let mut f = internal_next_prompt_fixture();
    let proposal = fixture::proposal(&f, 1);
    let response = fixture::response(&f, &proposal);
    let before = encode(&f.task, MAX_RECORD_BYTES).unwrap();
    {
        let tx = f.store.conn.transaction().unwrap();
        let mut changed = proposal.clone();
        changed["context_id"] = "context-v2:wrong".into();
        assert!(
            refuse_capacity(
                &tx,
                &f.task,
                &response,
                &changed,
                CapacityRefusalReason::PromptCapacity
            )
            .is_err()
        );
        // The caller drops the failed transaction; it never returns a successful
        // receipt or installs partially changed in-memory task state.
    }
    assert_eq!(encode(&f.task, MAX_RECORD_BYTES).unwrap(), before);
    assert_eq!(fixture::state(&f).count, 0);
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
}

#[test]
fn capacity_transition_uncommitted_success_has_no_reopen_effect() {
    let mut f = internal_next_prompt_fixture();
    let proposal = fixture::proposal(&f, 1);
    let response = fixture::response(&f, &proposal);
    let before = encode(&f.task, MAX_RECORD_BYTES).unwrap();
    {
        let tx = f.store.conn.transaction().unwrap();
        let _unacknowledged = refuse_capacity(
            &tx,
            &f.task,
            &response,
            &proposal,
            CapacityRefusalReason::PromptCapacity,
        )
        .unwrap();
        // Simulates failure before COMMIT, not power-loss durability or lost ACK.
    }
    assert_eq!(fixture::state(&f).count, 0);
    let saved: Vec<u8> = f
        .store
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [&f.basis.task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved, before);
}

#[test]
fn capacity_transition_injected_final_write_failure_rolls_back_owner_transaction() {
    // AC-0287: this explicitly injected failure is after the successful final
    // task write/capacity check. It does not simulate natural capacity exhaustion.
    let mut f = internal_next_prompt_fixture();
    let path = f.store.conn.path().unwrap().to_owned();
    let id = f.basis.task_id.clone();
    let proposal = fixture::proposal(&f, 1);
    let mut response = fixture::response(&f, &proposal);
    response.reported_input_tokens = Some(10);
    response.reported_output_tokens = Some(5);
    let before_task = saved_task_row(&f.store.conn, &id);
    let before_inputs = body_rows(&f.store.conn, "investigation_inputs", &id);
    let before_events = body_rows(&f.store.conn, "investigation_events", &id);
    let before_responses = body_rows(&f.store.conn, "investigation_responses", &id);
    assert_eq!(admission_namespace_count(&f.store.conn), 0);

    FAIL_AFTER_REFUSAL_WRITE.with(|failure| assert!(!failure.replace(true)));
    let outcome = f.store.refuse_next_prompt_capacity(
        &f._execution_guard,
        f.task.detail.summary.revision,
        &response,
        &proposal,
        CapacityRefusalReason::PromptCapacity,
    );
    // Reset even if a regression returns before the intended boundary, so the
    // thread-local hook cannot leak into another test on this worker thread.
    let unconsumed = FAIL_AFTER_REFUSAL_WRITE.with(|failure| failure.replace(false));
    assert!(!unconsumed, "the final-write failure hook was not reached");
    assert!(matches!(outcome, Err(InvestigationStoreError::Capacity)));

    let reopened = JobStore::open(&path).unwrap();
    assert_eq!(saved_task_row(&reopened.conn, &id), before_task);
    assert_eq!(
        body_rows(&reopened.conn, "investigation_inputs", &id),
        before_inputs
    );
    assert_eq!(
        body_rows(&reopened.conn, "investigation_events", &id),
        before_events
    );
    assert_eq!(
        body_rows(&reopened.conn, "investigation_responses", &id),
        before_responses
    );
    // Namespace creation, its refusal row and immutable triggers roll back too.
    assert_eq!(admission_namespace_count(&reopened.conn), 0);
    {
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let saved = storage::load_next_prompt_task_base(&tx, &id).unwrap();
        assert_eq!(saved.detail.usage, f.task.detail.usage);
        assert_eq!(saved.phase, Phase::Invocation);
        assert_eq!(
            admission_journal::derive_current(&tx, &saved)
                .unwrap()
                .count,
            0
        );
    }

    // The consumed hook does not poison retries, and the failed publication
    // did not charge the reported response usage before this successful retry.
    let next = f
        .store
        .refuse_next_prompt_capacity(
            &f._execution_guard,
            f.task.detail.summary.revision,
            &response,
            &proposal,
            CapacityRefusalReason::PromptCapacity,
        )
        .unwrap();
    assert_eq!(next.usage.reported_input_tokens, Some(10));
    assert_eq!(next.usage.reported_output_tokens, Some(5));
}

type BodyRows = Vec<(Vec<u8>, String)>;

fn body_rows(connection: &Connection, table: &str, id: &str) -> BodyRows {
    connection
        .prepare(&format!(
            "SELECT body,body_hash FROM {table} WHERE task_id=?1 ORDER BY body_hash,body"
        ))
        .unwrap()
        .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn saved_task_row(connection: &Connection, id: &str) -> (Vec<u8>, String, i64, i64) {
    connection
        .query_row(
            "SELECT record,record_hash,revision,live FROM investigation_tasks WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

fn admission_namespace_count(connection: &Connection) -> i64 {
    connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'investigation_admission_*'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

// Explicit cold @18 witnesses: no sealer, worker or provider is activated.
fn action_fixture() -> (completion::InternalFixture, Value) {
    action_fixture_for("domain-analyst@18")
}
fn action_fixture_for(role_name: &str) -> (completion::InternalFixture, Value) {
    use agents::investigation::{AcquisitionAdmission, acquisition_choices::AcquisitionChoices};
    let mut f = internal_next_prompt_fixture();
    let policy = &INVESTIGATION_V2_POLICY;
    f.request.specialist_id = role_name.into();
    let role: SpecialistId = serde_json::from_value(serde_json::json!(role_name)).unwrap();
    let intent = completion::host_fixture_intent(&f.request).unwrap();
    f.task.intent_hash = intent.request_hash.clone();
    f.task.completion_intent = Some(intent.clone());
    f.task.detail.completion = Some(completion::initial_detail(&intent));
    f.task.detail.summary.specialist_id = role;
    f.task.detail.specialist = role.definition().unwrap();
    f.metadata.request_hash = intent.request_hash.clone();
    let choices = AcquisitionChoices::derive(
        &intent.contract(),
        &f.metadata,
        &f.basis.task_id,
        &f.ledger,
        &[],
        policy,
    )
    .unwrap();
    let offered = serde_json::to_value(&choices.entries()[0].action).unwrap();
    let disclosure = f.step.disclosure.as_mut().unwrap();
    disclosure.completion_contract_hash =
        Some(intent.contract().fingerprint_with_policy(policy).unwrap());
    disclosure.completion_metadata_hash = Some(f.metadata.fingerprint_with_policy(policy).unwrap());
    let registry = agents::investigation::ProgressiveInvestigationContext::prepare(
        &intent.request_hash,
        &f.ledger,
        &[],
        policy,
    )
    .unwrap()
    .query_history;
    disclosure.query_history.as_mut().unwrap().request_hash = intent.request_hash.clone();
    disclosure.query_history.as_mut().unwrap().registry_hash = registry.registry_hash;
    disclosure.admission.as_mut().unwrap().acquisition = Some(AcquisitionAdmission {
        finish_patch: None,
        finish_policy_complete: false,
        capacity_exhausted: false,
        finish_reason: None,
        choices_hash: choices.stamp_hash().unwrap(),
        action_feedback_hash: None,
    });
    f.basis.request_hash = intent.request_hash;
    f.basis.completion_metadata_hash = f.metadata.fingerprint_with_policy(policy).unwrap();
    f.basis.admission = disclosure.admission.clone().unwrap();
    f.task.pending = Some(f.step.clone());
    f.task.admission_state.as_mut().unwrap().current_call = Some(f.basis.clone());
    let tx = f.store.conn.unchecked_transaction().unwrap();
    completion::rewrite_test_body(
        &tx,
        "investigation_invocations",
        &f.basis.task_id,
        "step_id",
        &f.step.step_id,
        &encode(&f.step, MAX_INVOCATION_BYTES).unwrap(),
    );
    fixture::save_task(&tx, &f.task);
    load_next_prompt_task(&tx, &f.basis.task_id).unwrap();
    tx.commit().unwrap();
    (f, offered)
}
fn action_next(f: &mut completion::InternalFixture) {
    let state = fixture::state(f);
    let ordinal = f.task.detail.usage.model_invocations + 1;
    f.step.step_id = format!("action-step:{ordinal}");
    let admission = f
        .step
        .disclosure
        .as_mut()
        .unwrap()
        .admission
        .as_mut()
        .unwrap();
    admission.consumed_budgets = f.task.detail.usage.clone();
    admission.capacity_refusal_count = state.count;
    admission.capacity_feedback = state.feedback;
    admission.acquisition.as_mut().unwrap().action_feedback_hash = state
        .action_feedback
        .as_ref()
        .map(|feedback| feedback.stamp_hash())
        .transpose()
        .unwrap();
    f.basis.step_id = f.step.step_id.clone();
    f.basis.invocation_ordinal = ordinal;
    f.basis.admission = admission.clone();
    f.task.pending = Some(f.step.clone());
    f.task.phase = Phase::Invocation;
    f.task.admission_state.as_mut().unwrap().current_call = Some(f.basis.clone());
    f.task.detail.usage.model_invocations += 1;
    f.task.detail.usage.generated_token_reservations += f.step.generated_tokens;
    let tx = f.store.conn.unchecked_transaction().unwrap();
    insert_body(
        &tx,
        "investigation_invocations",
        &f.basis.task_id,
        &f.step.step_id,
        &f.step,
        MAX_INVOCATION_BYTES,
    )
    .unwrap();
    bump(&tx, &mut f.task).unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ModelStarted,
        Some(&f.step.step_id),
        None,
        "Synthetic invocation; no provider executed.",
    )
    .unwrap();
    fixture::save_task(&tx, &f.task);
    tx.commit().unwrap();
}
#[test]
fn action_refusal_journal_preserves_capacity_bytes_and_atomic_feedback() {
    // AC-0482: real owner-checked writer and fresh durable reads, cold producer.
    use admission_journal::RefusalReason;
    use agents::investigation::ActionRefusalReason;
    let semantic = RefusalReason::Action(ActionRefusalReason::ActionNotOffered);
    let write = |f: &mut completion::InternalFixture, revision, value: &Value, reason| {
        let response = fixture::response(f, value);
        f.store
            .refuse_next_prompt(&f._execution_guard, revision, &response, value, reason)
    };
    let mut historical = internal_next_prompt_fixture();
    let revision = historical.task.detail.summary.revision;
    let proposal = fixture::proposal(&historical, 1);
    assert!(write(&mut historical, revision, &proposal, semantic).is_err());
    let (mut f, offered) = action_fixture();
    let before_inputs = body_rows(&f.store.conn, "investigation_inputs", &f.basis.task_id);
    let mut steps = vec![];
    let mut hashes = vec![];
    let mut semantic_sequence = 0;
    for index in 0..3 {
        if index != 0 {
            action_next(&mut f);
        }
        steps.push(f.step.step_id.clone());
        let proposal = fixture::proposal(&f, index + 1);
        hashes.push(core_prov::content_hash(
            &encode(&proposal, MAX_ACTION_BYTES).unwrap(),
        ));
        let revision = f.task.detail.summary.revision;
        let reason = if index == 1 {
            semantic
        } else {
            RefusalReason::Capacity(CapacityRefusalReason::PromptCapacity)
        };
        if index == 1 {
            let before = saved_task_row(&f.store.conn, &f.basis.task_id);
            assert!(write(&mut f, revision, &offered, semantic).is_err());
            assert_eq!(saved_task_row(&f.store.conn, &f.basis.task_id), before);
            FAIL_AFTER_REFUSAL_WRITE.with(|flag| flag.set(true));
            assert!(write(&mut f, revision, &proposal, semantic).is_err());
            assert!(!FAIL_AFTER_REFUSAL_WRITE.with(|flag| flag.replace(false)));
            assert_eq!(saved_task_row(&f.store.conn, &f.basis.task_id), before);
            assert!(fixture::state(&f).action_feedback.is_none());
        }
        write(&mut f, revision, &proposal, reason).unwrap(); // deliberately discard acknowledgement
        let reopened = JobStore::open(f.store.conn.path().unwrap()).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        f.task = load_next_prompt_task(&tx, &f.basis.task_id).unwrap();
        let state = admission_journal::derive_current(&tx, &f.task).unwrap();
        assert_eq!(state.count, [1, 1, 2][index]);
        assert_eq!(
            state.feedback.as_ref().unwrap().action_hash,
            hashes[if index < 2 { 0 } else { 2 }]
        );
        assert_eq!(f.task.detail.usage.model_invocations, index as u32 + 1);
        assert_eq!(
            (
                f.task.detail.usage.tool_actions,
                f.task.detail.usage.evidence_requests,
                f.task.detail.usage.evidence_bytes
            ),
            (0, 0, 0)
        );
        assert!(completion::attempts(&tx, &f.task).unwrap().is_empty());
        let acquired = load_ledger(&tx, &f.task).unwrap().unwrap();
        assert_eq!(
            acquired
                .queries
                .iter()
                .map(|page| page.response_bytes)
                .sum::<usize>(),
            0
        );
        assert!(acquired.queries.is_empty() && acquired.coverage_reads.is_empty());
        assert_eq!(
            f.task.detail.usage.generated_token_reservations,
            (index as u32 + 1) * f.step.generated_tokens
        );
        assert_eq!(
            body_rows(&tx, "investigation_inputs", &f.basis.task_id),
            before_inputs
        );
        if index == 1 {
            semantic_sequence = f.task.detail.summary.last_event_sequence;
        }
        if index != 0 {
            assert_eq!(
                state.action_feedback.as_ref().unwrap().action_hash,
                hashes[1]
            );
            assert_eq!(
                state.action_feedback.as_ref().unwrap().invocation_ordinal,
                2
            );
        }
        tx.commit().unwrap();
        assert!(write(&mut f, revision, &proposal, reason).is_err());
    }
    let tx = f.store.conn.unchecked_transaction().unwrap();
    for (sequence, expected_count) in [1, 1, 2].into_iter().enumerate() {
        let entry: admission_journal::Entry = read_body(
            &tx,
            "investigation_admission_refusals",
            "sequence",
            &f.basis.task_id,
            &(sequence as u32 + 1),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (entry.sequence, entry.count),
            (sequence as u32 + 1, expected_count)
        );
    }
    let at = admission_journal::derive_at(&tx, &f.task, &steps[2]).unwrap();
    assert_eq!(at.count, 1);
    assert_eq!(at.action_feedback.as_ref().unwrap().invocation_ordinal, 2);
    tx.commit().unwrap();
    // Move the refusal before ModelCompleted while preserving row hashes/keys.
    let tx = f.store.conn.unchecked_transaction().unwrap();
    let mut refused: InvestigationEvent = read_body(
        &tx,
        "investigation_events",
        "sequence",
        &f.basis.task_id,
        &signed(semantic_sequence).unwrap(),
    )
    .unwrap()
    .unwrap();
    let mut completed: InvestigationEvent = read_body(
        &tx,
        "investigation_events",
        "sequence",
        &f.basis.task_id,
        &signed(semantic_sequence - 1).unwrap(),
    )
    .unwrap()
    .unwrap();
    std::mem::swap(&mut refused.sequence, &mut completed.sequence);
    for event in [refused, completed] {
        completion::rewrite_test_body(
            &tx,
            "investigation_events",
            &f.basis.task_id,
            "sequence",
            &signed(event.sequence).unwrap(),
            &encode(&event, 16 * 1024).unwrap(),
        );
    }
    assert!(admission_journal::derive_current(&tx, &f.task).is_err());
    drop(tx);
    action_next(&mut f);
    fixture::accept_query(&mut f); // trusted publication fixture, not live @18 membership/dispatch
    let state = fixture::state(&f);
    assert_eq!(state.count, 2);
    assert!(state.feedback.is_none() && state.action_feedback.is_none());
    let tx = f.store.conn.unchecked_transaction().unwrap();
    assert!(
        admission_journal::derive_at(&tx, &f.task, &steps[2])
            .unwrap()
            .action_feedback
            .is_some()
    );
}

// AC-0260: a repetition notice cannot substitute for an actual acquired read.
#[test]
fn repeated_read_notice_rejects_unacquired_action_without_charging() {
    let (mut f, offered) = action_fixture();
    assert!(f.ledger.queries.is_empty() && f.ledger.coverage_reads.is_empty());
    let before = saved_task_row(&f.store.conn, &f.basis.task_id);
    let before_inputs = body_rows(&f.store.conn, "investigation_inputs", &f.basis.task_id);
    let response = fixture::response(&f, &offered);
    let revision = f.task.detail.summary.revision;
    assert!(
        f.store
            .refuse_next_prompt(
                &f._execution_guard,
                revision,
                &response,
                &offered,
                admission_journal::RefusalReason::Action(
                    agents::investigation::ActionRefusalReason::RepeatedRead
                ),
            )
            .is_err()
    );
    assert_eq!(saved_task_row(&f.store.conn, &f.basis.task_id), before);
    assert_eq!(
        body_rows(&f.store.conn, "investigation_inputs", &f.basis.task_id),
        before_inputs
    );
    assert!(fixture::state(&f).action_feedback.is_none());
}

#[test]
fn capacity_exclusion_durable_replay_preserves_analyst20_and_excludes_for_21() {
    for role in ["domain-analyst@20", "domain-analyst@21"] {
        let (mut f, offered) = action_fixture_for(role);
        let response = fixture::response(&f, &offered);
        let original_ledger = f.ledger.clone();
        let tx = f.store.conn.transaction().unwrap();
        let next = refuse_capacity(
            &tx,
            &f.task,
            &response,
            &offered,
            CapacityRefusalReason::PromptCapacity,
        )
        .unwrap();
        tx.commit().unwrap();
        f.task = next;
        assert_eq!(fixture::state(&f).count, 1);
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let choices = completion::SavedMetadataHistory::load(&tx, &f.task)
            .unwrap()
            .current_choices(&f.ledger, &f.metadata)
            .unwrap()
            .unwrap();
        let still_offered = choices
            .entries()
            .iter()
            .any(|entry| serde_json::to_value(&entry.action).unwrap() == offered);
        assert_eq!(still_offered, role == "domain-analyst@20");
        if role == "domain-analyst@21" {
            assert!(choices.entries().is_empty());
            assert!(choices.capacity_exhausted());
        }
        assert_eq!(f.ledger, original_ledger);
        let cutoff = f.basis.invocation_ordinal;
        assert!(
            admission_journal::capacity_exclusion_candidates(&tx, &f.task, cutoff, &f.ledger)
                .unwrap()
                .is_empty()
        );
        assert!(
            admission_journal::capacity_exclusion_candidates(
                &tx,
                &f.task,
                cutoff + 1,
                &InvestigationInputLedger {
                    revision: f.ledger.revision + 1,
                    ..f.ledger.clone()
                }
            )
            .unwrap()
            .is_empty()
        );
        let excluded =
            admission_journal::capacity_exclusion_candidates(&tx, &f.task, cutoff + 1, &f.ledger)
                .unwrap();
        assert_eq!(
            excluded.contains(&response.action_hash),
            role == "domain-analyst@21"
        );
        drop(choices);
        tx.commit().unwrap();
        // Reconstruct, seal and authenticate the next call from durable state.
        fixture::start_call(&mut f, &[], None);
        assert_eq!(fixture::state(&f).count, 1);
        assert_eq!(
            f.basis.admission.mode == CompletionCallMode::FinishOnly,
            role == "domain-analyst@21"
        );
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let trigger: String = tx.query_row(
            "SELECT sql FROM sqlite_schema WHERE name='investigation_admission_refusals_no_delete'",
            [], |row| row.get(0)).unwrap();
        tx.execute_batch("DROP TRIGGER investigation_admission_refusals_no_delete")
            .unwrap();
        tx.execute(
            "DELETE FROM investigation_admission_refusals WHERE task_id=?1",
            params![f.basis.task_id],
        )
        .unwrap();
        tx.execute_batch(&trigger).unwrap();
        assert!(admission_journal::derive_current(&tx, &f.task).is_err());
        // Roll back the corruption; missing history must never authenticate.
    }
}

// AC-1437 (#2522): @31 keeps a capacity-refused read withdrawn at later input
// revisions; @30 keeps the revision-local rule byte-for-byte.
#[test]
fn analyst31_capacity_exclusion_outlives_the_refused_revision() {
    for role in ["domain-analyst@30", "domain-analyst@31"] {
        let (mut f, offered) = action_fixture_for(role);
        let response = fixture::response(&f, &offered);
        let tx = f.store.conn.transaction().unwrap();
        let next = refuse_capacity(
            &tx,
            &f.task,
            &response,
            &offered,
            CapacityRefusalReason::PromptCapacity,
        )
        .unwrap();
        tx.commit().unwrap();
        f.task = next;
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let cutoff = f.basis.invocation_ordinal + 1;
        let same =
            admission_journal::capacity_exclusion_candidates(&tx, &f.task, cutoff, &f.ledger)
                .unwrap();
        assert!(same.contains(&response.action_hash), "{role}");
        let later = admission_journal::capacity_exclusion_candidates(
            &tx,
            &f.task,
            cutoff,
            &InvestigationInputLedger {
                revision: f.ledger.revision + 1,
                ..f.ledger.clone()
            },
        )
        .unwrap();
        assert_eq!(
            later.contains(&response.action_hash),
            role == "domain-analyst@31",
            "{role}"
        );
        // Never before the refused call, and never at an earlier revision.
        assert!(
            admission_journal::capacity_exclusion_candidates(
                &tx,
                &f.task,
                f.basis.invocation_ordinal,
                &InvestigationInputLedger {
                    revision: f.ledger.revision + 1,
                    ..f.ledger.clone()
                },
            )
            .unwrap()
            .is_empty()
        );
        if f.ledger.revision > 0 {
            assert!(
                admission_journal::capacity_exclusion_candidates(
                    &tx,
                    &f.task,
                    cutoff,
                    &InvestigationInputLedger {
                        revision: f.ledger.revision - 1,
                        ..f.ledger.clone()
                    },
                )
                .unwrap()
                .is_empty()
            );
        }
        drop(tx);
    }
}

// AC-0596: authenticated revision-local exclusion, historical @13 unchanged.
#[test]
fn auditor14_capacity_exclusion_reopens_without_ledger_credit() {
    for role in ["evidence-auditor@13", "evidence-auditor@14"] {
        let (mut f, offered) = action_fixture_for(role);
        let response = fixture::response(&f, &offered);
        let original_ledger = f.ledger.clone();
        let tx = f.store.conn.transaction().unwrap();
        let next = refuse_capacity(
            &tx,
            &f.task,
            &response,
            &offered,
            CapacityRefusalReason::PromptCapacity,
        )
        .unwrap();
        tx.commit().unwrap();
        f.task = next;
        assert_eq!(fixture::state(&f).count, 1);
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let choices = completion::SavedMetadataHistory::load(&tx, &f.task)
            .unwrap()
            .current_choices(&f.ledger, &f.metadata)
            .unwrap()
            .unwrap();
        let still_offered = choices
            .entries()
            .iter()
            .any(|entry| serde_json::to_value(&entry.action).unwrap() == offered);
        assert_eq!(still_offered, role == "evidence-auditor@13");
        if role == "evidence-auditor@14" {
            assert!(choices.entries().is_empty());
            assert!(choices.capacity_exhausted());
        }
        assert_eq!(f.ledger, original_ledger);
        let cutoff = f.basis.invocation_ordinal;
        assert!(
            admission_journal::capacity_exclusion_candidates(&tx, &f.task, cutoff, &f.ledger)
                .unwrap()
                .is_empty()
        );
        assert!(
            admission_journal::capacity_exclusion_candidates(
                &tx,
                &f.task,
                cutoff + 1,
                &InvestigationInputLedger {
                    revision: f.ledger.revision + 1,
                    ..f.ledger.clone()
                }
            )
            .unwrap()
            .is_empty()
        );
        let excluded =
            admission_journal::capacity_exclusion_candidates(&tx, &f.task, cutoff + 1, &f.ledger)
                .unwrap();
        assert_eq!(
            excluded.contains(&response.action_hash),
            role == "evidence-auditor@14"
        );
        drop(choices);
        tx.commit().unwrap();
        // Reconstruct, seal and authenticate the next call from durable state.
        fixture::start_call(&mut f, &[], None);
        assert_eq!(fixture::state(&f).count, 1);
        assert_eq!(
            f.basis.admission.mode == CompletionCallMode::FinishOnly,
            role == "evidence-auditor@14"
        );
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let trigger: String = tx.query_row(
            "SELECT sql FROM sqlite_schema WHERE name='investigation_admission_refusals_no_delete'",
            [], |row| row.get(0)).unwrap();
        tx.execute_batch("DROP TRIGGER investigation_admission_refusals_no_delete")
            .unwrap();
        tx.execute(
            "DELETE FROM investigation_admission_refusals WHERE task_id=?1",
            params![f.basis.task_id],
        )
        .unwrap();
        tx.execute_batch(&trigger).unwrap();
        assert!(admission_journal::derive_current(&tx, &f.task).is_err());
        // Roll back the corruption; missing history must never authenticate.
    }
}
#[test]
fn undecodable_native_calls_are_ledgered_refusals_capped_per_run() {
    // AC-1420: an @29 tool call that does not decode goes back to the model as
    // invalid_action feedback; the journal derives the count and refuses a
    // third, which the worker turns into the terminal decode stop.
    use admission_journal::RefusalReason;
    use agents::investigation::{ActionRefusalReason, MAX_INVALID_ACTION_REFUSALS};
    let invalid = RefusalReason::Action(ActionRefusalReason::InvalidAction);
    let proposal = serde_json::json!({"type": "refused_tool_call", "tool": "query_context",
        "refusal": "host_owned_field", "arguments": {"context_id": "x"}});
    let write = |f: &mut completion::InternalFixture, value: &Value, reason| {
        let revision = f.task.detail.summary.revision;
        let response = fixture::response(f, value);
        f.store
            .refuse_next_prompt(&f._execution_guard, revision, &response, value, reason)
    };
    // Earlier roles keep the terminal decode stop.
    let (mut old, _) = action_fixture();
    assert!(write(&mut old, &proposal, invalid).is_err());
    let (mut f, offered) = action_fixture_for("domain-analyst@29");
    // A proposal that decodes is never an invalid_action refusal.
    assert!(write(&mut f, &offered, invalid).is_err());
    for index in 0..=MAX_INVALID_ACTION_REFUSALS {
        if index != 0 {
            action_next(&mut f);
        }
        let result = write(&mut f, &proposal, invalid);
        if index == MAX_INVALID_ACTION_REFUSALS {
            assert_eq!(result.unwrap_err(), InvestigationStoreError::Capacity);
            break;
        }
        result.unwrap();
        let reopened = JobStore::open(f.store.conn.path().unwrap()).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        f.task = load_next_prompt_task(&tx, &f.basis.task_id).unwrap();
        let state = admission_journal::derive_current(&tx, &f.task).unwrap();
        let feedback = state.action_feedback.unwrap();
        assert_eq!(feedback.reason, ActionRefusalReason::InvalidAction);
        assert_eq!(feedback.invocation_ordinal, index as u32 + 1);
        assert_eq!(state.count, 0, "not a capacity refusal");
        assert_eq!(f.task.detail.usage.tool_actions, 0, "nothing ran");
        assert_eq!(f.task.detail.usage.model_invocations, index as u32 + 1);
        tx.commit().unwrap();
    }
}
