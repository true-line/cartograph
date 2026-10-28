use super::*;

// AC-0239: exercise the actual owned worker, firewall, durable transitions and
// saved result together. The scripted provider makes no network requests.
#[test]
fn qualified_worker_reads_frozen_inventory_and_reopens_without_source_credit() {
    let provider = Arc::new(Scripted::new(Mode::InventoryFinish));
    let fixture = Fixture::with_specialist(
        provider.as_ref(),
        tempfile::tempdir().unwrap(),
        "Explain the recorded analysis limits.",
        SpecialistId::DomainAnalystV6,
    );
    fixture.run(provider.clone());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "{detail:?}"
    );
    assert_eq!(detail.usage.tool_actions, 1);
    assert_eq!(detail.usage.evidence_requests, 0);
    let ledger = jobs.investigation_ledger(&fixture.id).unwrap().unwrap();
    assert_eq!(ledger.coverage_reads.len(), 1);
    assert!(ledger.citations.is_empty());
    let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
    assert_eq!(result.schema_version, 4);
    if let Some(path) = std::env::var_os("CARTOGRAPH_QUALIFIED_UI_FIXTURE") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&json!({
                "detail": detail, "result": result,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    assert!(result.inspection_assessment.is_none());
    assert_eq!(
        result.coverage.as_ref().unwrap().context_id,
        ledger.coverage.as_ref().unwrap().context_id
    );
    assert_eq!(
        provider.prompts.lock().unwrap()[1]["turn_progress"]["phase"],
        "after_coverage_read"
    );
    drop(jobs);
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_result(&fixture.id).unwrap().unwrap(),
        result
    );
    assert_eq!(
        reopened.investigation_ledger(&fixture.id).unwrap().unwrap(),
        ledger
    );
    assert_eq!(
        serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap()).unwrap(),
        fixture.graph_before
    );
}

#[test]
fn qualified_worker_supplies_same_qualification_through_graph_and_source_reads() {
    for required in [None, Some(agents::TaskRangeRole::DefinitionInitializer)] {
        let provider = Arc::new(Scripted::new(Mode::Finish));
        let fixture = Fixture::with_contract(
            provider.as_ref(),
            tempfile::tempdir().unwrap(),
            "Inspect the source under the recorded limits.",
            SpecialistId::EvidenceAuditorV6,
            required,
            ORIGINAL,
        );
        fixture.run(provider.clone());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        let state = fixture.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(
            detail.summary.status,
            InvestigationStatus::Completed,
            "{detail:?}"
        );
        let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
        assert_eq!(result.schema_version, 4);
        assert_eq!(result.inspection_assessment.is_some(), required.is_some());
        assert_eq!(detail.usage.evidence_requests, 1);
        let prompts = provider.prompts.lock().unwrap();
        for prompt in prompts.iter() {
            assert_eq!(prompt["context_id"], prompts[0]["context_id"]);
            assert_eq!(
                prompt["coverage_qualification"],
                prompts[0]["coverage_qualification"]
            );
            assert_eq!(prompt["coverage_qualification"]["exhaustive"], false);
        }
    }
}

#[test]
fn qualified_worker_rejects_wrong_context_before_tool_execution() {
    let provider = Arc::new(Scripted::new(Mode::WrongQualifiedContext));
    let fixture = Fixture::with_specialist(
        provider.as_ref(),
        tempfile::tempdir().unwrap(),
        "Inspect recorded scope.",
        SpecialistId::DomainAnalystV6,
    );
    fixture.run(provider.clone());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(detail.usage.tool_actions, 0);
    let ledger = jobs.investigation_ledger(&fixture.id).unwrap().unwrap();
    assert!(ledger.queries.is_empty());
    assert!(ledger.coverage_reads.is_empty());
    assert!(jobs.investigation_result(&fixture.id).unwrap().is_none());
}

// AC-0239: inventories that cannot fit the task never reach the model, and
// their presence is not silently reclassified as missing coverage.
#[test]
fn qualified_worker_refuses_inventory_capacity_before_model_dispatch() {
    let provider = Arc::new(Scripted::new(Mode::InventoryFinish));
    let fixture = Fixture::with_specialist(
        provider.as_ref(),
        tempfile::tempdir().unwrap(),
        "Explain the recorded limits.",
        SpecialistId::DomainAnalystV6,
    );
    let state = fixture.app.state::<AppState>();
    for index in 0..2 {
        let root = fixture
            ._directory
            .path()
            .join(format!("large-inventory-{index}"));
        std::fs::create_dir(&root).unwrap();
        let source = state.sources.lock().unwrap().register_local(&root).unwrap();
        let record = core_graph::coverage::CoverageRecord {
            repo_key: source.repo_key.clone(),
            registered_source_id: Some(source.source_id.clone()),
            ingest_id: format!("large-ingest-{index}"),
            config_hash: crate::coverage_config_hash(&[], crate::Registries::builtin()),
            inventory_json: json!({"inventory":"coverage@1","repo":source.repo_key,
                "commit":"workdir","layer_hints":[],"producers":[],"unsupported":[],
                "limited":false,"detail":"x".repeat(1_000_000)})
            .to_string(),
        };
        let mut graph = state.graph.lock().unwrap();
        let snapshot = graph.read_snapshot().unwrap();
        assert!(
            graph
                .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                    &snapshot,
                    &core_graph::GraphPatch::default(),
                    &source.repo_key,
                    &[],
                    Some(&record),
                    core_graph::source::SourceOmissions::NONE,
                )
                .unwrap()
        );
    }
    fixture.run(provider.clone());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        detail.error.as_deref(),
        Some("Investigation limit reached: host_limit.")
    );
    assert!(detail.coverage_context.is_none());
    assert!(jobs.investigation_ledger(&fixture.id).unwrap().is_none());
    assert!(jobs.investigation_result(&fixture.id).unwrap().is_none());
}
