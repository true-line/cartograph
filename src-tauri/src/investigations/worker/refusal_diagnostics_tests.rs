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
    let events = super::acceptance::ordered_events(&jobs, &fixture.id);
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
