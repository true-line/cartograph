use super::*;

#[test]
fn investigation_saved_policy_rejects_rehashed_tampering_after_reopen() {
    // AC-0188: a valid record hash does not authorize caller-selected limits.
    let mut fixture = Fixture::new();
    let detail = fixture
        .store
        .start_investigation(
            &request("saved-policy", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    let id = &detail.summary.investigation_id;
    let original: Vec<u8> = fixture
        .store
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    for mutation in ["unknown_profile", "more_calls", "less_tokens"] {
        let mut task: StoredTask = serde_json::from_slice(&original).unwrap();
        match mutation {
            "unknown_profile" => task.detail.limits.profile = "investigation-v2".into(),
            "more_calls" => task.detail.limits.model_invocations += 1,
            "less_tokens" => task.detail.limits.generated_token_reservations -= 1,
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&task).unwrap();
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
                params![id, bytes, core_prov::content_hash(&bytes)],
            )
            .unwrap();
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(
            reopened.investigation(id).unwrap_err(),
            InvestigationStoreError::Invalid,
            "{mutation}"
        );
    }
    fixture
        .store
        .conn
        .execute(
            "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
            params![id, original, core_prov::content_hash(&original)],
        )
        .unwrap();
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(id).unwrap().limits, detail.limits);
}

#[test]
fn investigation_pending_step_enforces_exact_policy_and_token_boundary() {
    // AC-0188: replay and dispatch use the same immutable per-call ceiling.
    let limits = InvestigationLimits::default();
    let mut pending = PendingInvestigationStep {
        step_id: "step:policy-boundary".into(),
        payload_hash: hash("payload"),
        input_ledger_hash: hash("ledger"),
        input_bytes: 1024,
        generated_tokens: limits.generated_tokens_per_invocation,
        disclosure: None,
    };
    assert!(validate_step(&pending, &limits).is_ok());
    pending.generated_tokens += 1;
    assert!(validate_step(&pending, &limits).is_err());
    pending.generated_tokens = 0;
    assert!(validate_step(&pending, &limits).is_err());
    pending.generated_tokens = 1;
    assert!(validate_step(&pending, &limits).is_ok());
    let mut forged = limits.clone();
    forged.generated_tokens_per_invocation += 1;
    assert!(validate_step(&pending, &forged).is_err());
    forged = limits.clone();
    forged.profile = "investigation-v2".into();
    assert!(validate_step(&pending, &forged).is_err());
}

#[test]
fn investigation_policy_rejects_excess_before_reservation_and_reopens_exact_step() {
    // AC-0188: an excess request leaves no invocation or reservation behind.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("policy-dispatch", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, false);
    let before = fixture.store.investigation(&id).unwrap();
    let mut pending = step(&ledger, 1);
    pending.generated_tokens = before.limits.generated_tokens_per_invocation + 1;
    assert_eq!(
        fixture
            .store
            .advance_investigation(
                &execution,
                before.summary.revision,
                InvestigationTransition::BeginInvocation {
                    step: pending.clone()
                },
            )
            .unwrap_err(),
        InvestigationStoreError::Invalid
    );
    let after = fixture.store.investigation(&id).unwrap();
    assert_eq!(after.summary.revision, before.summary.revision);
    assert_eq!(after.usage, before.usage);
    let count: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM investigation_invocations WHERE task_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    pending.generated_tokens = before.limits.generated_tokens_per_invocation;
    let started = begin(&mut fixture, &id, &execution, pending);
    assert_eq!(started.usage.model_invocations, 1);
    assert_eq!(started.usage.generated_token_reservations, 2048);
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&id).unwrap().usage, started.usage);
}

#[test]
fn investigation_policy_journal_reserves_cancel_and_terminal_events() {
    // AC-0188: exercise the production journal primitive in isolation. Repeated
    // admission metadata fills capacity; it does not model a provider/tool run.
    let mut fixture = Fixture::new();
    let (id, _execution) = fixture.start("policy-journal", InvestigationProviderMode::Local);
    let append = |connection: &Connection, task: &mut StoredTask, kind| {
        event(
            connection,
            task,
            kind,
            None,
            None,
            "Journal boundary fixture",
        )
    };
    let limit;
    {
        let tx = fixture.store.conn.transaction().unwrap();
        let mut task = load_task_base(&tx, &id).unwrap();
        limit = journal_event_limit(&task).unwrap();
        assert_eq!(limit, 96);
        let mut forged = load_task_base(&tx, &id).unwrap();
        forged.detail.limits.tool_actions += 1;
        assert_eq!(
            append(&tx, &mut forged, InvestigationEventKind::ActionAdmitted).unwrap_err(),
            InvestigationStoreError::Invalid
        );
        assert_eq!(
            forged.detail.summary.last_event_sequence,
            task.detail.summary.last_event_sequence
        );
        let revision = task.detail.summary.revision;
        while task.detail.summary.last_event_sequence < limit - 3 {
            append(&tx, &mut task, InvestigationEventKind::ActionAdmitted).unwrap();
        }
        assert_eq!(
            append(&tx, &mut task, InvestigationEventKind::ActionAdmitted).unwrap_err(),
            InvestigationStoreError::Capacity
        );
        assert_eq!(task.detail.summary.last_event_sequence, 93);
        save_task(&tx, &task, revision, false).unwrap();
        tx.commit().unwrap();
    }
    {
        let tx = fixture.store.conn.transaction().unwrap();
        let mut task = load_task_base(&tx, &id).unwrap();
        append(&tx, &mut task, InvestigationEventKind::CancelRequested).unwrap();
        assert_eq!(task.detail.summary.last_event_sequence, 94);
        assert_eq!(
            append(&tx, &mut task, InvestigationEventKind::CancelRequested).unwrap_err(),
            InvestigationStoreError::Capacity
        );
        // Isolate phase-specific reservation admission without synthesizing a
        // terminal task/result. The transaction and these rows are rolled back.
        task.phase = Phase::Terminal;
        append(&tx, &mut task, InvestigationEventKind::ResultPersisted).unwrap();
        append(&tx, &mut task, InvestigationEventKind::Cancelled).unwrap();
        assert_eq!(task.detail.summary.last_event_sequence, limit);
        assert_eq!(
            append(&tx, &mut task, InvestigationEventKind::Cancelled).unwrap_err(),
            InvestigationStoreError::Capacity
        );
        let rows: i64 = tx
            .query_row(
                "SELECT count(*) FROM investigation_events WHERE task_id=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, signed(limit).unwrap());
        tx.rollback().unwrap();
    }
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation(&id)
            .unwrap()
            .summary
            .last_event_sequence,
        93
    );
    let tail = reopened.investigation_events(&id, 92).unwrap();
    assert_eq!(tail.items.len(), 1);
    assert_eq!(tail.items[0].sequence, 93);
    assert!(
        reopened
            .investigation_events(&id, limit)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        reopened.investigation_events(&id, limit + 1).unwrap_err(),
        InvestigationStoreError::Invalid
    );
}
