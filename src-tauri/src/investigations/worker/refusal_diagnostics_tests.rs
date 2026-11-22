//! AC-0184 (#937): ordinary terminal diagnostics retain classes, never rejected prose.
use super::*;

pub(super) const METADATA: &str = "This deliberately long graph explanation remains replay protected across ordinary finishing responses.";

pub(super) fn assert_saved_refusal(fixture: &Fixture, id: &str, expected: &str, rejected: &str) {
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(detail.error.as_deref(), Some(expected));
    assert!(!detail.summary.has_result);
    assert!(jobs.investigation_result(id).unwrap().is_none());
    let mut events = Vec::new();
    let mut after = 0;
    loop {
        let page = jobs.investigation_events(id, after).unwrap();
        events.extend(page.items);
        if !page.has_more {
            break;
        }
        assert!(page.next_sequence > after);
        after = page.next_sequence;
    }
    let terminal: Vec<_> = events
        .iter()
        .filter(|e| e.kind == InvestigationEventKind::Failed)
        .collect();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].summary, expected);
    assert!(
        !events
            .iter()
            .any(|e| e.kind == InvestigationEventKind::ResultPersisted)
    );
    let public = serde_json::to_string(&(&detail, &events)).unwrap();
    assert!(
        !public.contains(rejected),
        "rejected prose must not become a diagnostic"
    );
    drop(jobs);
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(id).unwrap(), detail);
    assert!(reopened.investigation_result(id).unwrap().is_none());
    assert_eq!(reopened.investigation_events(id, 0).unwrap().items, events);
}

#[test]
fn investigation_metadata_second_replay_is_terminal_after_one_correction() {
    let provider = Arc::new(Scripted::new(Mode::MetadataReplay));
    let fixture = Fixture::with_prepared_contract(
        provider.as_ref(),
        tempfile::tempdir().unwrap(),
        "Inspect the unresolved boundary.",
        SpecialistId::DomainAnalyst,
        None,
        ORIGINAL,
        None,
        |_, state, _| {
            let provenance = core_prov::Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Gap,
                vec![],
                "synthetic-refusal",
                METADATA.as_bytes(),
            )
            .unwrap();
            state
                .graph
                .lock()
                .unwrap()
                .put_node(&core_graph::Node {
                    id: "gap:diagnostic".into(),
                    label: "Gap".into(),
                    props: json!({"reason":METADATA, "prov":provenance}),
                })
                .unwrap();
        },
    );
    fixture.run(provider.clone());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        provider.prompts.lock().unwrap()[2]["finish_correction"]["refusal"],
        "source_replay"
    );
    assert_saved_refusal(
        &fixture,
        &fixture.id,
        "Investigation finish refused: source_replay.",
        METADATA,
    );
}

#[test]
fn investigation_metadata_source_replay_can_finish_once_with_fixed_feedback() {
    let provider = Arc::new(Scripted::new(Mode::MetadataReplayCorrection));
    let fixture = Fixture::with_prepared_contract(
        provider.as_ref(),
        tempfile::tempdir().unwrap(),
        "Inspect the unresolved boundary.",
        SpecialistId::DomainAnalyst,
        None,
        ORIGINAL,
        None,
        |_, state, _| {
            let provenance = core_prov::Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Gap,
                vec![],
                "synthetic-refusal",
                METADATA.as_bytes(),
            )
            .unwrap();
            state
                .graph
                .lock()
                .unwrap()
                .put_node(&core_graph::Node {
                    id: "gap:diagnostic".into(),
                    label: "Gap".into(),
                    props: json!({"reason":METADATA, "prov":provenance}),
                })
                .unwrap();
        },
    );
    fixture.run(provider.clone());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        provider.prompts.lock().unwrap()[2]["finish_correction"]["refusal"],
        "source_replay"
    );
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
    assert_eq!(
        result.findings[0].statement,
        "The recorded graph metadata supports this limited observation."
    );
    let events = super::ordered_events(&jobs, &fixture.id);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == InvestigationEventKind::SourceReplayRefused)
            .count(),
        1
    );
    assert!(
        !serde_json::to_string(&(&detail, &result, &events))
            .unwrap()
            .contains(METADATA)
    );
}

#[test]
fn investigation_unadmitted_read_has_fixed_terminal_diagnostic_without_acquisition() {
    let provider = Arc::new(Scripted::new(Mode::UnadmittedRead));
    let fixture = Fixture::new(provider.as_ref());
    let state = fixture.app.state::<AppState>();
    let before = state
        .jobs
        .lock()
        .unwrap()
        .investigation(&fixture.id)
        .unwrap()
        .usage;
    fixture.run(provider.clone());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_saved_refusal(
        &fixture,
        &fixture.id,
        "Investigation read refused: unadmitted_fact.",
        "node:unadmitted",
    );
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .investigation(&fixture.id)
        .unwrap();
    assert_eq!(detail.usage.evidence_requests, before.evidence_requests + 1);
    assert_eq!(detail.usage.evidence_items, before.evidence_items);
    assert_eq!(detail.usage.evidence_bytes, before.evidence_bytes);
    assert_eq!(
        detail.usage.captured_validation_bytes,
        before.captured_validation_bytes
    );
}

// AC-0184/0187: cancellation changes the durable revision while the actual
// rejected response is in flight. Its known refusal/usage must still settle.
#[test]
fn investigation_source_replay_cancellation_race_retains_refusal_and_measured_usage() {
    for clear_job in [false, true] {
        let provider = Arc::new(Scripted::new(Mode::MetadataReplayCancelled));
        let fixture = Fixture::with_prepared_contract(
            provider.as_ref(),
            tempfile::tempdir().unwrap(),
            "Inspect the unresolved boundary.",
            SpecialistId::DomainAnalyst,
            None,
            ORIGINAL,
            None,
            |_, state, _| {
                let provenance = core_prov::Provenance::new(
                    Tier::Deterministic,
                    ConfidenceTier::Gap,
                    vec![],
                    "synthetic-refusal",
                    METADATA.as_bytes(),
                )
                .unwrap();
                state
                    .graph
                    .lock()
                    .unwrap()
                    .put_node(&core_graph::Node {
                        id: "gap:diagnostic".into(),
                        label: "Gap".into(),
                        props: json!({"reason":METADATA, "prov":provenance}),
                    })
                    .unwrap();
            },
        );
        fixture.cancel_inside_call(provider.as_ref(), clear_job);
        fixture.run(provider.clone());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        let state = fixture.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Cancelled);
        assert!(!detail.summary.invocation_pending);
        assert!(!detail.summary.has_result);
        assert_eq!(detail.usage.model_invocations, 2);
        assert_eq!(detail.usage.reported_input_tokens, Some(200));
        assert_eq!(detail.usage.reported_output_tokens, Some(80));
        // AC-0418: cancellation retains both real reports, including refused finish.
        assert_eq!(detail.usage.reported_cache_read_tokens, Some(14));
        let events = super::ordered_events(&jobs, &fixture.id);
        let refusals: Vec<_> = events
            .iter()
            .filter(|event| event.kind == InvestigationEventKind::SourceReplayRefused)
            .collect();
        assert_eq!(refusals.len(), 1);
        let refusal_sequence = refusals[0].sequence;
        assert!(events.iter().any(
            |event| event.kind == InvestigationEventKind::CancelRequested
                && event.sequence < refusal_sequence
        ));
        assert!(
            events
                .iter()
                .any(|event| event.kind == InvestigationEventKind::Cancelled
                    && event.sequence > refusal_sequence)
        );
        assert!(
            !events
                .iter()
                .any(|event| event.kind == InvestigationEventKind::ModelStarted
                    && event.sequence > refusal_sequence)
        );
        assert!(
            !serde_json::to_string(&(&detail, &events))
                .unwrap()
                .contains(METADATA)
        );
        assert!(jobs.investigation_result(&fixture.id).unwrap().is_none());
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }
}
