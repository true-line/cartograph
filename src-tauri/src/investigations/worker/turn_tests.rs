use super::*;

fn fixture(provider: &Scripted) -> Fixture {
    Fixture::with_specialist(
        provider,
        tempfile::tempdir().unwrap(),
        "Inspect retained guard evidence and its limits.",
        SpecialistId::DomainAnalystV4,
    )
}

// AC-0260/T0260: actual worker admission and the visible raw budget share one limit.
#[test]
fn v4_repeated_query_budget_matches_actual_host_limit_and_reopens() {
    let provider = Arc::new(Scripted::new(Mode::RepeatQuery));
    let fixture = fixture(&provider);
    fixture.run(provider.clone());
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        detail.error.as_deref(),
        Some("Investigation limit reached: host_limit.")
    );
    let ledger = jobs.investigation_ledger(&fixture.id).unwrap().unwrap();
    let before = jobs.investigation_turn_basis(&fixture.id).unwrap();
    let prompts = provider.prompts.lock().unwrap();
    assert_eq!(prompts.len(), ledger.queries.len() + 1);
    assert_eq!(detail.usage.tool_actions as usize, prompts.len());
    let mut consumed = 0;
    for (index, prompt) in prompts.iter().enumerate() {
        let turn = &prompt["turn_progress"];
        assert_eq!(
            turn["raw_query_budget"]["limit_bytes"],
            MAX_TOTAL_QUERY_BYTES
        );
        assert_eq!(turn["raw_query_budget"]["consumed_bytes"], consumed);
        assert_eq!(
            turn["raw_query_budget"]["remaining_bytes"],
            MAX_TOTAL_QUERY_BYTES - consumed
        );
        if index > 1 {
            assert_eq!(turn["last_completed_action"]["no_progress"], true);
        }
        if let Some(query) = ledger.queries.get(index) {
            consumed += query.response_bytes;
        }
    }
    assert!(consumed <= MAX_TOTAL_QUERY_BYTES);
    assert!(consumed + ledger.queries.last().unwrap().response_bytes > MAX_TOTAL_QUERY_BYTES);
    assert_eq!(detail.usage.evidence_requests, 0);
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_turn_basis(&fixture.id).unwrap(),
        before
    );
    assert_eq!(before.completed_tool, Some(InvestigationTool::QueryContext));
}

#[test]
fn v4_read_order_repeated_reads_and_failed_read_are_durable() {
    for mode in [Mode::Finish, Mode::RepeatRead, Mode::UnavailableRead] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = fixture(&provider);
        fixture.run(provider.clone());
        let state = fixture.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let basis = jobs.investigation_turn_basis(&fixture.id).unwrap();
        let prompts = provider.prompts.lock().unwrap();
        assert_eq!(prompts[0]["turn_progress"]["phase"], "initial");
        assert_eq!(prompts[1]["turn_progress"]["phase"], "after_query");
        if matches!(mode, Mode::UnavailableRead) {
            assert_eq!(prompts.len(), 2);
            assert_eq!(
                jobs.investigation(&fixture.id).unwrap().summary.status,
                InvestigationStatus::Failed
            );
            assert_eq!(basis.completed_tool, Some(InvestigationTool::QueryContext));
        } else {
            assert_eq!(prompts[2]["turn_progress"]["phase"], "after_read");
            assert_eq!(basis.completed_tool, Some(InvestigationTool::ReadEvidence));
            if matches!(mode, Mode::RepeatRead) {
                assert_eq!(
                    prompts[3]["turn_progress"]["last_completed_action"]["previously_acquired"],
                    true
                );
            }
        }
        let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
        assert_eq!(
            reopened.investigation_turn_basis(&fixture.id).unwrap(),
            basis
        );
    }
}

#[test]
fn v4_owner_recovery_preserves_durable_progress_without_resuming() {
    let provider = Arc::new(Scripted::new(Mode::PanicAfterQuery));
    let fixture = fixture(&provider);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || fixture.run(provider.clone())
        ))
        .is_err()
    );
    let state = fixture.app.state::<AppState>();
    let before = state
        .jobs
        .lock()
        .unwrap()
        .investigation_turn_basis(&fixture.id)
        .unwrap();
    assert_eq!(before.completed_tool, Some(InvestigationTool::QueryContext));
    drop(fixture.execution);
    let candidate = state
        .jobs
        .lock()
        .unwrap()
        .investigation_recovery_candidates()
        .unwrap()
        .remove(0);
    let reservation = state
        .job_execution_locks
        .try_reserve(candidate.lock_target())
        .unwrap();
    let recovered = state
        .jobs
        .lock()
        .unwrap()
        .recover_investigation(&candidate, reservation)
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.summary.status,
        InvestigationStatus::OutcomeUnknown
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_turn_basis(&fixture.id).unwrap(),
        before
    );
    assert_eq!(
        reopened.investigation(&fixture.id).unwrap().summary.status,
        InvestigationStatus::OutcomeUnknown
    );
}

#[test]
fn v4_durable_basis_rejects_event_input_disagreement_after_reopen() {
    let provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = fixture(&provider);
    fixture.run(provider);
    let connection = rusqlite::Connection::open(fixture.app_data.join("state.db")).unwrap();
    let mut rows = connection.prepare("SELECT sequence,body FROM investigation_events WHERE task_id=?1 ORDER BY sequence DESC").unwrap();
    let events = rows
        .query_map([&fixture.id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let (sequence, mut event) = events
        .into_iter()
        .map(|(sequence, bytes)| (sequence, serde_json::from_slice::<Value>(&bytes).unwrap()))
        .find(|(_, event)| event["kind"] == "tool_completed")
        .unwrap();
    assert_eq!(event["tool"], "read_evidence");
    event["tool"] = json!("query_context");
    let event: InvestigationEvent = serde_json::from_value(event).unwrap();
    let body = serde_json::to_vec(&event).unwrap();
    let trigger: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='investigation_events_no_update'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute_batch("DROP TRIGGER investigation_events_no_update")
        .unwrap();
    connection
        .execute(
            "UPDATE investigation_events SET body=?1,body_hash=?2 WHERE task_id=?3 AND sequence=?4",
            rusqlite::params![body, content_hash(&body), fixture.id, sequence],
        )
        .unwrap();
    connection.execute_batch(&trigger).unwrap();
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_turn_basis(&fixture.id),
        Err(crate::jobs::investigations::InvestigationStoreError::Invalid)
    );
}
