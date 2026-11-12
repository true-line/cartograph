//! AC-0287: actual next-family parent results are mandatory bounded T3 history.
use super::*;

fn parent(large: bool) -> (NextFixture, Arc<NextProvider>) {
    let mut scripted = NextProvider::new(false);
    scripted.source_scenario = Some(SourceScenario::Acquired);
    scripted.extra_inventory = true;
    scripted.large_finish = large;
    let provider = Arc::new(scripted);
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV10);
    f.run(provider.clone());
    let state = f.base.base.app.state::<AppState>();
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .investigation_owned_detail(&f.execution, &f.id)
        .unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "{:?}",
        detail.error
    );
    assert_eq!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::Supported)
    );
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 4);
    (f, provider)
}

fn request(parent: &NextFixture) -> StagedObligationRequest {
    let state = parent.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs
        .investigation_owned_detail(&parent.execution, &parent.id)
        .unwrap();
    let mut request = parent.request.clone();
    request.request_nonce = "next-prompt-worker-child".into();
    request.specialist_id = "evidence-auditor@10".into();
    request.parent_id = Some(parent.id.clone());
    request.conversation_id = Some(detail.summary.conversation_id);
    request
}

#[test]
fn next_prompt_worker_parent_child_preserves_exact_saved_result_and_mandatory_history_hash() {
    // AC-0578: replay the whole genuine >16KiB result under the new child policy.
    let (parent, _) = parent(true);
    let request = request(&parent);
    let expected_context_id = request.expected_context_id.clone();
    let mut scripted = NextProvider::new(false);
    scripted.metadata_order = Some([false, false]);
    let provider = Arc::new(scripted);
    let state = parent.base.base.app.state::<AppState>();
    let mut jobs = state.jobs.lock().unwrap();
    let expected = jobs
        .investigation_next_parent_history_with_policy(
            &parent.id,
            request.conversation_id.as_deref().unwrap(),
            &INVESTIGATION_V2_POLICY.with_parent_history_32768().unwrap(),
        )
        .unwrap();
    let expected_history = expected.history().to_vec();
    let expected_parent = jobs
        .investigation_owned_detail(&parent.execution, &parent.id)
        .unwrap();
    let saved_connection =
        rusqlite::Connection::open(parent.base.base.app_data.join("state.db")).unwrap();
    let saved_result: Vec<u8> = saved_connection
        .query_row(
            "SELECT body FROM investigation_results WHERE task_id=?1",
            [&parent.id],
            |row| row.get(0),
        )
        .unwrap();
    let expected_result: Value = serde_json::from_slice(&saved_result).unwrap();
    let start = jobs
        .start_next_prompt_investigation(
            &request,
            Some(&descriptor(provider.as_ref(), InvestigationProviderMode::Local).unwrap()),
        )
        .unwrap();
    let id = start.detail.summary.investigation_id;
    assert_eq!(
        start.detail.summary.parent_id.as_deref(),
        Some(parent.id.as_str())
    );
    assert_eq!(
        start.detail.summary.conversation_id,
        expected_parent.summary.conversation_id
    );
    let plan = jobs
        .claim_plan(
            start.detail.summary.job_id,
            crate::jobs::ClaimMode::StartQueued,
        )
        .unwrap();
    let lock = state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .unwrap();
    let (_, execution) = jobs.claim_execution(&plan, lock).unwrap();
    drop(jobs);
    let live = state.investigations.insert(&id).unwrap();
    run_completion(
        parent.base.base.app.handle().clone(),
        id.clone(),
        request,
        execution.clone(),
        provider.clone(),
        live,
        Instant::now(),
    );
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation_owned_detail(&execution, &id).unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "{:?}",
        detail.error
    );
    assert_eq!(
        detail.summary.specialist_id,
        SpecialistId::EvidenceAuditorV10
    );
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 3);
    let prompts = provider.observed.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 3);
    for prompt in prompts.iter() {
        assert_eq!(prompt["context_id"], expected_context_id);
        assert_eq!(
            prompt["prior_findings_as_t3_history"],
            serde_json::to_value(&expected_history).unwrap()
        );
        let history: Value =
            serde_json::from_str(prompt["prior_findings_as_t3_history"][0].as_str().unwrap())
                .unwrap();
        assert_eq!(history["saved_result"], expected_result);
        assert_eq!(history["parent_investigation_id"], parent.id);
        assert_eq!(
            history["original_graph_snapshot_id"],
            expected_parent.summary.graph_snapshot_id.clone().unwrap()
        );
        assert_eq!(history["saved_execution_status"], "completed");
        assert_eq!(
            history["authority"],
            "Prior specialist findings remain T3/InferredWeak; execution status does not establish complete knowledge."
        );
    }
    drop(jobs);
    let connection =
        rusqlite::Connection::open(parent.base.base.app_data.join("state.db")).unwrap();
    let mut statement = connection
        .prepare("SELECT body FROM investigation_inputs WHERE task_id=?1")
        .unwrap();
    let encoded = serde_json::to_vec(&expected_history).unwrap();
    for body in statement
        .query_map([&id], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
    {
        let ledger: InvestigationInputLedger = serde_json::from_slice(&body.unwrap()).unwrap();
        assert_eq!(
            ledger.coverage.as_ref().unwrap().context_id,
            expected_context_id
        );
        assert_eq!(ledger.supplied_history_bytes, encoded.len());
        assert_eq!(
            ledger.supplied_history_hash.as_deref(),
            Some(content_hash(&encoded).as_str())
        );
    }
    let reopened = crate::jobs::JobStore::open(parent.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_owned_detail(&execution, &id)
            .unwrap(),
        detail
    );
}

#[test]
fn next_prompt_worker_versioned_history_admits_large_saved_parent() {
    let (parent, _) = parent(true);
    let request = request(&parent);
    let provider = Arc::new(NextProvider::new(false));
    let state = parent.base.base.app.state::<AppState>();
    // The parent is a real complete result produced by the actual worker, not
    // inflated storage bytes or an edited checksum. Its complete body is kept.
    let connection =
        rusqlite::Connection::open(parent.base.base.app_data.join("state.db")).unwrap();
    let result: Vec<u8> = connection
        .query_row(
            "SELECT body FROM investigation_results WHERE task_id=?1",
            [&parent.id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(result.len() > 16 * 1024);
    let counts = || {
        connection
            .query_row(
                "SELECT (SELECT count(*) FROM investigation_tasks),(SELECT count(*) FROM jobs)",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap()
    };
    let before = counts();
    let mut jobs = state.jobs.lock().unwrap();
    assert!(matches!(
        jobs.investigation_next_parent_history(
            &parent.id,
            request.conversation_id.as_deref().unwrap()
        ),
        Err(InvestigationStoreError::Capacity)
    ));
    // AC-0578: the historical reader refuses, but a fresh versioned child admits it.
    let started = jobs
        .start_next_prompt_investigation(
            &request,
            Some(&descriptor(provider.as_ref(), InvestigationProviderMode::Local).unwrap()),
        )
        .unwrap();
    assert_eq!(
        started.detail.limits.history_policy,
        Some(agents::investigation::HistoryPolicy::ParentHistory32768V1)
    );
    assert_eq!(counts(), (before.0 + 1, before.1 + 1));
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 0);
    let unchanged: Vec<u8> = connection
        .query_row(
            "SELECT body FROM investigation_results WHERE task_id=?1",
            [&parent.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        unchanged, result,
        "overflow neither truncates nor replaces saved parent history"
    );
}

#[path = "hosted_history_tests.rs"]
mod hosted_history_tests;

pub(in crate::investigations) use hosted_history_tests::full_hosted_auditor_provider;

#[path = "auditor14_copied_parent_tests.rs"]
mod auditor14_copied_parent_tests;
