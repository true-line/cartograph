use super::*;
use agents::TaskRangeRole;
const UTF8_SOURCE: &str = "// café — retained source\nexport function enabled(ok: boolean) { const limited = ok === false; if (limited) return false; }\n";
fn fixture(provider: &Scripted, role: TaskRangeRole) -> Fixture {
    Fixture::with_contract(
        provider,
        tempfile::tempdir().unwrap(),
        "Inspect the selected retained source and its limits.",
        SpecialistId::DomainAnalystV5,
        Some(role),
        UTF8_SOURCE,
    )
}

// AC-0261/0262: real parser receipts, retained bytes, coordinator and exact locations.
#[test]
fn inspection_actual_captured_target_finishes_with_bound_assessment_and_location() {
    let provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = fixture(&provider, TaskRangeRole::DefinitionInitializer);
    let target = fixture.request.inspection_requirements.as_ref().unwrap()[0].clone();
    assert_ne!(
        target.receipt_inventory_index, target.index,
        "viewer ordinal is not protocol index"
    );
    let state = fixture.app.state::<AppState>();
    assert_eq!(
        state
            .jobs
            .lock()
            .unwrap()
            .investigation_receipt_references(&target.registered_source_id)
            .unwrap()
            .len(),
        1,
        "queued requirements already retain receipts"
    );
    fixture.run(provider.clone());
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    assert_eq!(
        detail.summary.inspection_status,
        Some(InvestigationInspectionStatus::Supplied)
    );
    assert_eq!(result.schema_version, 3);
    let assessment = result.inspection_assessment.as_ref().unwrap();
    assert_eq!(detail.inspection_assessment.as_ref(), Some(assessment));
    assert_eq!(assessment.requirements.len(), 1);
    assert!(
        assessment.requirements[0].acquired
            && assessment.requirements[0].supplied
            && assessment.requirements[0].cited
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    drop(jobs);
    let location = crate::investigations::inspection::location(&state, &target).unwrap();
    assert_eq!(location.start_line, 2);
    assert_eq!(location.column_unit, "utf8_bytes");
    assert!(location.end_exclusive);
    let before = &UTF8_SOURCE.as_bytes()[..target.captured.byte_start as usize];
    let column = before.len() - before.iter().rposition(|b| *b == b'\n').unwrap();
    assert_eq!(location.start_column, column as u64);
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
    assert_eq!(
        reopened.investigation_result(&fixture.id).unwrap(),
        Some(result)
    );
}

#[test]
fn inspection_partial_wrong_range_and_repeated_reads_keep_truthful_coverage() {
    for (mode, role, acquired, supplied) in [
        (
            Mode::MetadataFinish,
            TaskRangeRole::DefinitionInitializer,
            false,
            false,
        ),
        (Mode::Finish, TaskRangeRole::RuleExit, false, false),
        (
            Mode::RepeatRead,
            TaskRangeRole::DefinitionInitializer,
            true,
            true,
        ),
    ] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = fixture(&provider, role);
        fixture.run(provider.clone());
        let state = fixture.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Completed);
        let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
        assert!(!result.findings.is_empty());
        let assessment = result.inspection_assessment.unwrap();
        assert_eq!(assessment.requirements.len(), 1);
        let observed = &assessment.requirements[0];
        assert_eq!(observed.acquired, acquired);
        assert_eq!(observed.supplied, supplied);
        assert_eq!(
            detail.summary.inspection_status,
            Some(if supplied {
                InvestigationInspectionStatus::Supplied
            } else {
                InvestigationInspectionStatus::Unmet
            })
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            if matches!(mode, Mode::MetadataFinish) {
                2
            } else if matches!(mode, Mode::RepeatRead) {
                4
            } else {
                3
            }
        );
    }
}

#[test]
fn inspection_resolver_rejects_role_rebinding_and_start_replay_precedes_freshness() {
    let provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = fixture(&provider, TaskRangeRole::DefinitionInitializer);
    let state = fixture.app.state::<AppState>();
    let target = fixture.request.inspection_requirements.as_ref().unwrap()[0].clone();
    let (resolved, _) = crate::investigations::inspection::resolve(
        &state,
        &target.fact,
        &target.graph_snapshot_id,
        &target.receipt_id,
        target.receipt_inventory_index as usize,
    )
    .unwrap();
    assert_eq!(resolved, target);
    let mut changed = fixture.request.clone();
    changed.request_nonce = "other-selection".into();
    let targets = changed.inspection_requirements.as_mut().unwrap();
    targets[0].role = TaskRangeRole::ConditionExpression;
    targets[0].target_id = targets[0].content_id().unwrap();
    // @5 is retired (AC-1062); this checks its own rebinding refusal.
    let error = crate::investigations::commands::with_retired_starts(|| {
        crate::investigations::commands::start(
            changed,
            fixture.app.handle().clone(),
            Instant::now(),
        )
    })
    .unwrap_err();
    assert!(matches!(
        error,
        crate::investigations::commands::InvestigationStartError::Rejected {
            code: "refresh_required",
            definitive: true,
            ..
        }
    ));
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&core_graph::Node {
            id: "node:uncited-change".into(),
            label: "Function".into(),
            props: json!({}),
        })
        .unwrap();
    assert!(
        crate::investigations::inspection::resolve(
            &state,
            &target.fact,
            &target.graph_snapshot_id,
            &target.receipt_id,
            target.receipt_inventory_index as usize
        )
        .is_err()
    );
    let replay = crate::investigations::commands::start(
        fixture.request.clone(),
        fixture.app.handle().clone(),
        Instant::now(),
    )
    .unwrap();
    assert_eq!(replay.investigation_id, fixture.id);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn inspection_tampered_assessment_cannot_surface_through_detail_history_or_result() {
    let provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = fixture(&provider, TaskRangeRole::DefinitionInitializer);
    fixture.run(provider);
    let connection = rusqlite::Connection::open(fixture.app_data.join("state.db")).unwrap();
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [&fixture.id],
            |row| row.get(0),
        )
        .unwrap();
    let original = String::from_utf8(bytes).unwrap();
    assert!(original.contains("\"inspection_status\":\"supplied\""));
    let changed = original
        .replace(
            "\"inspection_status\":\"supplied\"",
            "\"inspection_status\":\"unmet\"",
        )
        .replace("\"status\":\"supplied\"", "\"status\":\"unmet\"")
        .replace("\"supplied\":true", "\"supplied\":false");
    connection
        .execute(
            "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
            rusqlite::params![
                changed.as_bytes(),
                content_hash(changed.as_bytes()),
                fixture.id
            ],
        )
        .unwrap();
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    assert!(reopened.investigation(&fixture.id).is_err());
    assert!(reopened.investigation_result(&fixture.id).is_err());
    assert!(reopened.investigation_history(None, None).is_err());
}

// AC-0261: closing without a finish is not an observation of required source.
#[test]
fn inspection_terminal_without_result_is_not_assessed_and_reopens() {
    for (mode, expected) in [
        (Mode::Malformed, InvestigationStatus::Failed),
        (Mode::Unknown, InvestigationStatus::OutcomeUnknown),
        (Mode::CancelTool, InvestigationStatus::Cancelled),
    ] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = fixture(&provider, TaskRangeRole::DefinitionInitializer);
        let state = fixture.app.state::<AppState>();
        let mut queued = fixture.request.clone();
        queued.request_nonce = "cancel-before-claim".into();
        let mut jobs = state.jobs.lock().unwrap();
        let queued = jobs
            .start_investigation(
                &queued,
                Some(&descriptor(provider.as_ref(), InvestigationProviderMode::Local).unwrap()),
            )
            .unwrap()
            .detail;
        let cancelled = jobs
            .cancel_investigation(&queued.summary.investigation_id)
            .unwrap();
        assert_eq!(
            cancelled.summary.inspection_status,
            Some(InvestigationInspectionStatus::NotAssessed)
        );
        assert!(cancelled.inspection_assessment.is_none());
        drop(jobs);
        if matches!(mode, Mode::CancelTool) {
            let app = fixture.app.handle().clone();
            let id = fixture.id.clone();
            *provider.cancel.lock().unwrap() = Some(Box::new(move || {
                let state = app.state::<AppState>();
                let detail = state
                    .jobs
                    .lock()
                    .unwrap()
                    .cancel_investigation(&id)
                    .unwrap();
                assert!(detail.summary.invocation_pending);
                assert_eq!(
                    detail.summary.inspection_status,
                    Some(InvestigationInspectionStatus::Pending)
                );
            }));
        }
        fixture.run(provider);
        let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
        let detail = reopened.investigation(&fixture.id).unwrap();
        assert_eq!(detail.summary.status, expected);
        assert_eq!(
            detail.summary.inspection_status,
            Some(InvestigationInspectionStatus::NotAssessed)
        );
        assert!(detail.inspection_assessment.is_none());
        assert!(
            reopened
                .investigation_result(&fixture.id)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            reopened
                .investigation(&queued.summary.investigation_id)
                .unwrap(),
            cancelled
        );
    }
}

// AC-0262: exact historical locations do not depend on current graph association.
#[test]
fn inspection_historical_location_is_pinned_and_errors_are_sanitized() {
    let provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = fixture(&provider, TaskRangeRole::DefinitionInitializer);
    let target = fixture.request.inspection_requirements.as_ref().unwrap()[0].clone();
    let state = fixture.app.state::<AppState>();
    let before = crate::investigations::inspection::location(&state, &target).unwrap();
    let mut graph = state.graph.lock().unwrap();
    let agents::TaskFactKey::Node { id } = &target.fact else {
        panic!("expected actual BusinessRule node")
    };
    graph.delete_node(id).unwrap();
    drop(graph);
    let after = crate::investigations::inspection::location(&state, &target).unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    let mut forged = target;
    forged.source.repo = "unrelated/retained-repo".into();
    forged.target_id = forged.content_id().unwrap();
    forged.validate().unwrap();
    let error = crate::investigations::inspection::location(&state, &forged).unwrap_err();
    assert_eq!(
        error,
        "Captured inspection target changed or is unavailable; refresh and select the source again."
    );
}

// AC-0280: a callable that owns no rule resolves through production inspection
// to its exact retained declaration span, as a valid source obligation target.
#[test]
fn inspection_resolves_rule_less_declaration_receipt() {
    const SOURCE: &str = "export function guard(ok: boolean) { if (!ok) return false; return true; }\nexport function total(a: number, b: number) { return a + b; }\n";
    let directory = tempfile::tempdir().unwrap();
    let (state, _app_data, _) = captured_fixture_state(directory.path(), SOURCE, false, None);
    let node = state
        .graph
        .lock()
        .unwrap()
        .nodes_with_label("Symbol")
        .unwrap()
        .into_iter()
        .find(|node| node.props["name"] == "total")
        .unwrap();
    let fact = agents::TaskFactKey::Node { id: node.id };
    let description =
        crate::primary_source::describe(&state, &crate::task_evidence::graph_fact(&fact)).unwrap();
    assert_eq!(description.ranges.len(), 1);
    let (target, _guard) = crate::investigations::inspection::resolve(
        &state,
        &fact,
        &description.graph_snapshot_id,
        &description.receipt_id,
        description.ranges[0].index,
    )
    .unwrap();
    assert_eq!((target.role, target.index), (TaskRangeRole::Provenance, 0));
    let span = &SOURCE[target.captured.byte_start as usize..target.captured.byte_end as usize];
    assert!(span.starts_with("function total(") && span.ends_with('}'));
    agents::investigation::Obligation::Source {
        obligation_id: String::new(),
        target: Box::new(target),
    }
    .content_id()
    .unwrap();
}
