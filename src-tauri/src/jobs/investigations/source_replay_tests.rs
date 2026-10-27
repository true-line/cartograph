use super::*;

fn refuse(
    fixture: &mut Fixture,
    id: &str,
    execution: &JobExecution,
) -> Result<InvestigationDetail, InvestigationStoreError> {
    let detail = fixture.store.investigation(id)?;
    fixture
        .store
        .advance_investigation(execution, detail.summary.revision, refusal())
}

fn refusal() -> InvestigationTransition {
    InvestigationTransition::SourceReplayRefused {
        step_id: "step:1".into(),
        reported_input_tokens: Some(10),
        reported_output_tokens: Some(20),
        reported_cache_read_tokens: Some(7),
        active_milliseconds: 100,
    }
}

fn running() -> (Fixture, String, JobExecution, InvestigationInputLedger) {
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("source-replay", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, false);
    begin(&mut fixture, &id, &execution, step(&ledger, 1));
    (fixture, id, execution, ledger)
}

fn change_task(fixture: &mut Fixture, id: &str, change: impl FnOnce(&mut StoredTask)) {
    let mut task = storage::load_task(&fixture.store.conn, id).unwrap();
    let revision = task.detail.summary.revision;
    change(&mut task);
    storage::save_task(
        &fixture.store.conn,
        &task,
        revision,
        task.phase == Phase::Terminal,
    )
    .unwrap();
}

fn append(
    fixture: &mut Fixture,
    id: &str,
    kind: InvestigationEventKind,
    tool: Option<InvestigationTool>,
) {
    let mut task = storage::load_task(&fixture.store.conn, id).unwrap();
    let revision = task.detail.summary.revision;
    storage::event(
        &fixture.store.conn,
        &mut task,
        kind,
        Some("step:1"),
        tool,
        "Fixture event.",
    )
    .unwrap();
    storage::save_task(&fixture.store.conn, &task, revision, false).unwrap();
}

// AC-0184: immutable refusal/start ordering permits one distinct call, without
// persisting the rejected response or allowing stale transitions/tools.
#[test]
fn source_replay_transaction_consumes_once_and_reopens_without_response_hashes() {
    let (mut fixture, id, execution, ledger) = running();
    let old = fixture.store.investigation(&id).unwrap();
    let ready = refuse(&mut fixture, &id, &execution).unwrap();
    assert_eq!(ready.summary.status, InvestigationStatus::Running);
    assert_eq!(ready.usage.reported_input_tokens, Some(10));
    assert_eq!(ready.usage.reported_cache_read_tokens, Some(7));
    for table in ["investigation_responses", "investigation_results"] {
        let count: i64 = fixture
            .store
            .conn
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE task_id=?1"),
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "rejected output persisted in {table}");
    }
    assert_eq!(
        fixture
            .store
            .advance_investigation(&execution, old.summary.revision, refusal())
            .unwrap_err(),
        InvestigationStoreError::Stale
    );
    assert_eq!(
        refuse(&mut fixture, &id, &execution).unwrap_err(),
        InvestigationStoreError::Stale
    );
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&id).unwrap(), ready);
    let correction = begin(&mut fixture, &id, &execution, step(&ledger, 2));
    assert_eq!(correction.usage.model_invocations, 2);
    for transition in [
        InvestigationTransition::BeginInvocation {
            step: step(&ledger, 3),
        },
        InvestigationTransition::ActionAdmitted {
            response: response("step:2", 200),
            tool: Some(InvestigationTool::QueryContext),
            result: None,
        },
    ] {
        assert!(
            fixture
                .store
                .advance_investigation(&execution, correction.summary.revision, transition)
                .is_err()
        );
        assert_eq!(fixture.store.investigation(&id).unwrap(), correction);
    }
}

// AC-0184: a known refusal terminalizes using existing reserved capacity;
// cancellation wins and measured usage survives either terminal path.
#[test]
fn source_replay_known_refusal_terminalizes_at_cancel_budget_and_capacity() {
    for case in 0..4 {
        let (mut fixture, id, execution, _) = running();
        match case {
            0 => {
                fixture.store.cancel_investigation(&id).unwrap();
            }
            1 => change_task(&mut fixture, &id, |task| {
                task.detail.usage.generated_token_reservations =
                    task.detail.limits.generated_token_reservations;
            }),
            2 => {
                let mut task = storage::load_task(&fixture.store.conn, &id).unwrap();
                let revision = task.detail.summary.revision;
                let maximum = storage::journal_event_limit(&task).unwrap() - 3;
                while task.detail.summary.last_event_sequence < maximum {
                    storage::event(
                        &fixture.store.conn,
                        &mut task,
                        InvestigationEventKind::ModelCompleted,
                        Some("step:1"),
                        None,
                        "Capacity fixture.",
                    )
                    .unwrap();
                }
                storage::save_task(&fixture.store.conn, &task, revision, false).unwrap();
            }
            _ => {
                // Existing capacity-fixture technique: occupied immutable bytes,
                // not a raised ceiling or a fabricated correction allowance.
                let remaining = storage::remaining_task_capacity(&fixture.store.conn, &id).unwrap();
                fixture.store.conn.execute(
                    "INSERT INTO investigation_responses VALUES (?1,'capacity-padding',zeroblob(?2),?3)",
                    params![id, (remaining - 64 - 256) as i64, "0".repeat(64)],
                ).unwrap();
            }
        }
        let before = fixture.store.investigation(&id).unwrap();
        let terminal = refuse(&mut fixture, &id, &execution).unwrap();
        assert_eq!(
            terminal.summary.status,
            if case == 0 {
                InvestigationStatus::Cancelled
            } else {
                InvestigationStatus::Failed
            }
        );
        assert_eq!(terminal.usage.model_invocations, 1);
        assert_eq!(terminal.usage.reported_input_tokens, Some(10));
        assert_eq!(terminal.usage.reported_output_tokens, Some(20));
        if case != 0 {
            assert_eq!(
                terminal.error.as_deref(),
                Some("Investigation finish refused: source_replay.")
            );
        }
        assert_eq!(
            terminal.summary.last_event_sequence,
            before.summary.last_event_sequence + 2
        );
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&id).unwrap(), terminal);
        let task = storage::load_task(&fixture.store.conn, &id).unwrap();
        assert!(task.pending.is_none());
        assert!(
            fixture
                .store
                .advance_investigation(
                    &execution,
                    terminal.summary.revision,
                    InvestigationTransition::BeginInvocation {
                        step: step(&ledger(false), 2)
                    }
                )
                .is_err()
        );
    }
}

// Test-only corruption keeps the immutable trigger/schema intact afterward.
fn rewrite<T: Serialize>(
    fixture: &Fixture,
    table: &str,
    key: &str,
    id: &str,
    value: &T,
    sequence: i64,
) {
    let trigger = format!("{table}_no_update");
    let sql: String = fixture
        .store
        .conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name=?1",
            [&trigger],
            |r| r.get(0),
        )
        .unwrap();
    fixture
        .store
        .conn
        .execute_batch(&format!("DROP TRIGGER {trigger}"))
        .unwrap();
    let body = serde_json::to_vec(value).unwrap();
    fixture
        .store
        .conn
        .execute(
            &format!("UPDATE {table} SET body=?1,body_hash=?2 WHERE task_id=?3 AND {key}=?4"),
            params![body, core_prov::content_hash(&body), id, sequence],
        )
        .unwrap();
    fixture.store.conn.execute_batch(&sql).unwrap();
}

// AC-0184: saved validation checks SQL/body/task parity and invocation binding,
// not display prose. Duplicate refusals and post-refusal tool activity fail shut.
#[test]
fn source_replay_saved_state_rejects_cross_task_sequence_ledger_and_duplicate_records() {
    for case in 0..7 {
        let (mut fixture, id, execution, _) = running();
        let ready = refuse(&mut fixture, &id, &execution).unwrap();
        if case < 4 {
            let sequence = ready.summary.last_event_sequence as i64;
            let (body, hash): (Vec<u8>, String) = fixture.store.conn.query_row(
                "SELECT body,body_hash FROM investigation_events WHERE task_id=?1 AND sequence=?2", params![id, sequence], |r| Ok((r.get(0)?, r.get(1)?)),
            ).unwrap();
            let mut event: InvestigationEvent = storage::decode(&body, &hash).unwrap();
            match case {
                0 => event.investigation_id = "inv:foreign".into(),
                1 => event.sequence += 1,
                2 => event.step_id = Some("step:foreign".into()),
                _ => event.kind = InvestigationEventKind::ModelStarted,
            }
            rewrite(
                &fixture,
                "investigation_events",
                "sequence",
                &id,
                &event,
                sequence,
            );
        } else if case == 4 {
            let mut invoked: PendingInvestigationStep = storage::read_body(
                &fixture.store.conn,
                "investigation_invocations",
                "step_id",
                &id,
                &"step:1",
            )
            .unwrap()
            .unwrap();
            invoked.input_ledger_hash = hash("foreign-ledger");
            let sql: String = fixture.store.conn.query_row("SELECT sql FROM sqlite_master WHERE name='investigation_invocations_no_update'", [], |r| r.get(0)).unwrap();
            fixture
                .store
                .conn
                .execute_batch("DROP TRIGGER investigation_invocations_no_update")
                .unwrap();
            let body = serde_json::to_vec(&invoked).unwrap();
            fixture.store.conn.execute("UPDATE investigation_invocations SET body=?1,body_hash=?2 WHERE task_id=?3 AND step_id='step:1'", params![body, core_prov::content_hash(&body), id]).unwrap();
            fixture.store.conn.execute_batch(&sql).unwrap();
        } else {
            append(
                &mut fixture,
                &id,
                if case == 5 {
                    InvestigationEventKind::SourceReplayRefused
                } else {
                    InvestigationEventKind::ToolStarted
                },
                (case == 6).then_some(InvestigationTool::QueryContext),
            );
        }
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert!(
            reopened.investigation(&id).is_err(),
            "accepted corrupt case {case}"
        );
    }
}

// AC-0184: a durable opportunity never introduces owner-death resumption.
#[test]
fn source_replay_recovery_interrupts_ready_and_never_replays_started_correction() {
    for correction_started in [false, true] {
        let (mut fixture, id, execution, ledger) = running();
        refuse(&mut fixture, &id, &execution).unwrap();
        if correction_started {
            begin(&mut fixture, &id, &execution, step(&ledger, 2));
        }
        let candidate = fixture
            .store
            .investigation_recovery_candidates()
            .unwrap()
            .remove(0);
        drop(execution);
        let reservation = fixture.locks.try_reserve(candidate.lock_target()).unwrap();
        let terminal = fixture
            .store
            .recover_investigation(&candidate, reservation)
            .unwrap()
            .unwrap();
        assert_eq!(
            terminal.summary.status,
            if correction_started {
                InvestigationStatus::OutcomeUnknown
            } else {
                InvestigationStatus::Interrupted
            }
        );
        assert_eq!(
            terminal.usage.model_invocations,
            if correction_started { 2 } else { 1 }
        );
        assert!(!terminal.summary.has_result);
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&id).unwrap(), terminal);
    }
}
