use super::*;

#[test]
fn progressive_worker_supported_refused_and_reopened_results_keep_new_family() {
    // AC-0286 / T-0286: real host transitions retain request6/prompt8/result6.
    for (role, mode, calls, refusals) in [
        (
            SpecialistId::DomainAnalystV9,
            Mode::CompletionSupported,
            4,
            0,
        ),
        (
            SpecialistId::EvidenceAuditorV9,
            Mode::CompletionRepair,
            5,
            1,
        ),
    ] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = CompletionFixture::new(provider.as_ref(), role, true, true, false);
        assert_eq!(fixture.request.schema_version, 6);
        fixture.run(provider.clone());
        let state = fixture.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(
            detail.summary.status,
            InvestigationStatus::Completed,
            "{:?}",
            detail.error
        );
        assert_eq!(detail.summary.schema_version, 6);
        assert_eq!(detail.limits, INVESTIGATION_V2_POLICY.limits());
        assert_eq!(detail.completion.as_ref().unwrap().refusal_count, refusals);
        let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
        assert_eq!(result.schema_version, 6);
        // Optional UI artifact is an actual scripted host result, not provider
        // acceptance. It contains the unchanged production wire DTOs.
        if role == SpecialistId::DomainAnalystV9
            && let Some(path) = std::env::var_os("CARTOGRAPH_PROGRESSIVE_UI_FIXTURE")
        {
            let mut events = Vec::new();
            let mut after = 0;
            loop {
                let page = jobs.investigation_events(&fixture.id, after).unwrap();
                events.extend(page.items);
                if !page.has_more {
                    break;
                }
                assert!(page.next_sequence > after);
                after = page.next_sequence;
            }
            assert_eq!(events.len() as u64, detail.summary.last_event_sequence);
            std::fs::write(
                path,
                serde_json::to_vec_pretty(
                    &json!({"detail":detail,"result":result,"events":events}),
                )
                .unwrap(),
            )
            .unwrap();
        }
        assert!(
            result
                .disclosure_hash
                .as_ref()
                .unwrap()
                .starts_with("investigation-disclosure-v5:")
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
        assert!(
            provider
                .prompts
                .lock()
                .unwrap()
                .iter()
                .all(|p| p["schema_version"] == 8)
        );
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(fixture.base.app_data.join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
        assert_eq!(
            reopened.investigation_result(&fixture.id).unwrap(),
            Some(result)
        );
    }
}

#[test]
fn progressive_worker_historical_replay_runs_exact_counted_query_and_reopens() {
    // AC-0286 / T-0286: replay is a paid ordinary acquisition, not a lookup.
    let provider = Arc::new(Scripted::new(Mode::ProgressiveReplay));
    let fixture = CompletionFixture::new(
        provider.as_ref(),
        SpecialistId::DomainAnalystV9,
        true,
        true,
        false,
    );
    fixture.run(provider.clone());
    let state = fixture.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "{:?}",
        detail.error
    );
    assert_eq!(detail.usage.tool_actions, 5);
    assert_eq!(detail.usage.model_invocations, 6);
    assert_eq!(detail.usage.reported_input_tokens, Some(600));
    assert_eq!(detail.usage.reported_output_tokens, Some(240));
    let ledger = jobs.investigation_ledger(&fixture.id).unwrap().unwrap();
    assert_eq!(ledger.queries.len(), 3);
    assert_eq!(ledger.queries[2].query, ledger.queries[0].query);
    assert_ne!(ledger.queries[2].query, ledger.queries[1].query);
    assert_eq!(
        ledger.queries[2].response_hash,
        ledger.queries[0].response_hash
    );
    let prompts = provider.prompts.lock().unwrap();
    let after_replay = &prompts[3];
    assert_eq!(
        after_replay["compact_context"]["query_history"]["prior_queries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        after_replay["compact_context"]["query_history"]["latest_query"]["selection"],
        serde_json::to_value(&ledger.queries[0].query).unwrap()
    );
    assert!(
        after_replay["turn_progress"]["last_completed_action"]
            .get("query")
            .is_none()
    );
    let consumed: usize = ledger.queries.iter().map(|q| q.response_bytes).sum();
    assert_eq!(
        after_replay["turn_progress"]["raw_query_budget"]["consumed_bytes"],
        json!(consumed)
    );
    let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
    assert_eq!(result.schema_version, 6);
    drop(jobs);
    let reopened = crate::jobs::JobStore::open(fixture.base.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_ledger(&fixture.id).unwrap(),
        Some(ledger)
    );
    assert_eq!(
        reopened.investigation_result(&fixture.id).unwrap(),
        Some(result)
    );
}

#[test]
fn progressive_worker_invalid_replay_stops_before_tool_or_raw_budget_charge() {
    // AC-0286 / T-0286: context/index/digest mismatch cannot execute a selector.
    for mode in [
        Mode::ProgressiveReplayWrongContext,
        Mode::ProgressiveReplayWrongIndex,
        Mode::ProgressiveReplayWrongId,
    ] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = CompletionFixture::new(
            provider.as_ref(),
            SpecialistId::DomainAnalystV9,
            true,
            true,
            false,
        );
        fixture.run(provider.clone());
        let state = fixture.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Failed);
        assert_eq!(detail.usage.tool_actions, 2);
        assert_eq!(detail.usage.model_invocations, 3);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        let ledger = jobs.investigation_ledger(&fixture.id).unwrap().unwrap();
        assert_eq!(ledger.queries.len(), 2);
        assert_eq!(ledger.revision, 2);
        assert!(jobs.investigation_result(&fixture.id).unwrap().is_none());
    }
}

#[test]
fn progressive_worker_refusal_cancellation_and_unknown_never_add_hidden_retry() {
    // AC-0286 / T-0286: the new family preserves existing terminal precedence.
    for (mode, status, calls) in [
        (Mode::CompletionNoAttempt, InvestigationStatus::Failed, 2),
        (
            Mode::CompletionCancelRefused,
            InvestigationStatus::Cancelled,
            2,
        ),
        (Mode::Unknown, InvestigationStatus::OutcomeUnknown, 1),
    ] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = CompletionFixture::new(
            provider.as_ref(),
            SpecialistId::DomainAnalystV9,
            true,
            false,
            false,
        );
        if matches!(mode, Mode::CompletionCancelRefused) {
            let app = fixture.base.app.handle().clone();
            let id = fixture.id.clone();
            *provider.cancel.lock().unwrap() = Some(Box::new(move || {
                app.state::<AppState>()
                    .jobs
                    .lock()
                    .unwrap()
                    .cancel_investigation(&id)
                    .unwrap();
            }));
        }
        fixture.run(provider.clone());
        let state = fixture.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(detail.summary.status, status, "{:?}", detail.error);
        assert!(jobs.investigation_result(&fixture.id).unwrap().is_none());
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(fixture.base.app_data.join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
    }
}
