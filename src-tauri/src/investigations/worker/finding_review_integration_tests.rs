//! Synthetic production worker acceptance; no live provider or owner data.
use super::*;

type DatabaseRows = Vec<(String, Vec<Vec<rusqlite::types::Value>>)>;

fn stored_rows(path: &std::path::Path) -> DatabaseRows {
    assert!(
        path.is_file(),
        "fixture database must already exist: {}",
        path.display()
    );
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let names = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name != 'investigation_finding_reviews' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let quoted = name.replace('"', "\"\"");
            let columns = connection
                .prepare(&format!("SELECT * FROM \"{quoted}\" LIMIT 0"))
                .unwrap()
                .column_count();
            let order = (1..=columns)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let mut statement = connection
                .prepare(&format!("SELECT * FROM \"{quoted}\" ORDER BY {order}"))
                .unwrap();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|i| row.get(i))
                        .collect::<rusqlite::Result<Vec<rusqlite::types::Value>>>()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            (name, rows)
        })
        .collect()
}

fn durable_inputs(fixture: &Fixture) -> Vec<DatabaseRows> {
    [
        "state.db",
        "graph.db",
        "proposals.sqlite",
        "retained-source/receipts.sqlite",
        "retained-source/captures.sqlite",
    ]
    .into_iter()
    .map(|path| stored_rows(&fixture.app_data.join(path)))
    .collect()
}

fn cited_review_fixture(provider: &Scripted) -> Fixture {
    Fixture::with_prepared_contract(
        provider,
        tempfile::tempdir().unwrap(),
        "Inspect the available guard evidence and its limits.",
        SpecialistId::DomainAnalystV3,
        None,
        ORIGINAL,
        None,
        |_, state, _| {
            let provenance = core_prov::Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Gap,
                vec![],
                "synthetic-review-fixture",
                b"unobserved external boundary",
            )
            .unwrap();
            state
                .graph
                .lock()
                .unwrap()
                .put_node(&core_graph::Node {
                    id: "gap:review-boundary".into(),
                    label: "Gap".into(),
                    props: json!({"reason":"unresolved external boundary", "prov":provenance}),
                })
                .unwrap();
        },
    )
}

// AC-0296 / T-0296: real coordinator + scripted provider + retained capture;
// review remains metadata and cannot mutate graph/export/history or run work.
#[test]
fn finding_review_scripted_worker_preserves_graph_export_and_source_boundaries() {
    let provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = cited_review_fixture(provider.as_ref());
    fixture.run(provider.clone());
    let state = fixture.app.state::<AppState>();
    let (result, ledger, detail, references, jobs_before) = {
        let jobs = state.jobs.lock().unwrap();
        let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
        let ledger = jobs.investigation_ledger(&fixture.id).unwrap().unwrap();
        let source_id = &ledger.receipt_references[0].source_id;
        (
            result,
            ledger.clone(),
            jobs.investigation(&fixture.id).unwrap(),
            jobs.investigation_receipt_references(source_id).unwrap(),
            serde_json::to_value(jobs.list().unwrap()).unwrap(),
        )
    };
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert_eq!(detail.usage.evidence_requests, 1);
    assert_eq!(
        detail.usage.captured_validation_bytes,
        ORIGINAL.len() as u64
    );
    assert_eq!(result.findings.len(), 1);
    let citation_id = &result.findings[0].citation_ids[0];
    let historical_before =
        crate::investigations::history::read(&state, &fixture.id, &ledger, citation_id).unwrap();
    assert_eq!(
        historical_before.status,
        InvestigationCitationStatus::Available
    );
    assert_eq!(historical_before.text.as_deref(), Some("ok === false"));
    // The fixture removed the checkout before execution. Recreate that path
    // with different bytes; they cannot become this saved citation's evidence.
    std::fs::create_dir(fixture._directory.path().join("target")).unwrap();
    std::fs::write(
        fixture._directory.path().join("target/rule.ts"),
        "export const replaced = true;\n",
    )
    .unwrap();
    let checkout_counter_before = state.sources.lock().unwrap().checkout_access_attempts();
    let replacement = crate::source_access::with_registered_read(
        &state.sources,
        &historical_before.citation.source.as_ref().unwrap().repo,
        |root| std::fs::read_to_string(root.join("rule.ts")).map_err(|e| e.to_string()),
    )
    .unwrap();
    assert_eq!(replacement, "export const replaced = true;\n");
    assert_eq!(
        state.sources.lock().unwrap().checkout_access_attempts(),
        checkout_counter_before + 1,
        "counter observes a real current-checkout read separately from retained inspection"
    );
    let export_before = serde_json::to_vec(
        &crate::export_spec_blocking(
            spec::ExportMode::BestEffort,
            &fixture.app.state::<AppState>(),
        )
        .unwrap(),
    )
    .unwrap();
    let graph_before = state.graph.lock().unwrap().read_snapshot().unwrap();
    assert!(
        graph_before
            .0
            .iter()
            .any(|node| node.props["prov"]["confidence_tier"] == "Confirmed"),
        "the real parser seeded a confirmed fact"
    );
    assert!(
        graph_before
            .0
            .iter()
            .any(|node| node.label == "Gap" && node.props["prov"]["confidence_tier"] == "Gap"),
        "the fixture includes a real graph Gap, not a display-only placeholder"
    );
    let bundle = crate::build_spec_bundle(
        &state.graph.lock().unwrap(),
        &state.decisions.lock().unwrap(),
        &crate::curated::CuratedReview::default(),
        spec::ExportMode::BestEffort,
    )
    .unwrap();
    assert!(
        bundle
            .artifacts
            .iter()
            .flat_map(|artifact| &artifact.assertions)
            .any(|assertion| assertion.provenance.confidence_tier == ConfidenceTier::Confirmed)
    );
    assert!(
        bundle
            .artifacts
            .iter()
            .flat_map(|artifact| &artifact.assertions)
            .any(|assertion| assertion.subject_id == "gap:review-boundary"
                && assertion.provenance.confidence_tier == ConfidenceTier::Gap
                && assertion.omission.is_some())
    );
    let durable_before = durable_inputs(&fixture);
    let calls_before = provider.calls.load(Ordering::SeqCst);
    let retained_before = state.primary_sources.evidence_access_attempts();
    let checkout_before = state.sources.lock().unwrap().checkout_access_attempts();
    assert!(
        retained_before > 0,
        "counter observes real worker/historical retained leases"
    );
    let prompts_before = provider.prompts.lock().unwrap().clone();
    let copied_before = provider.copied.lock().unwrap().clone();

    let mut jobs = state.jobs.lock().unwrap();
    let initial = jobs
        .investigation_finding_reviews(&fixture.id, &result.result_id)
        .unwrap();
    assert_eq!(initial.reviews.len(), 1);
    assert_eq!(initial.reviews[0].revision, 0);
    assert_eq!(durable_inputs(&fixture), durable_before);
    let mut observation = initial.reviews[0].clone();
    for (revision, decision) in [
        FindingReviewDecision::AcceptedForFollowUp,
        FindingReviewDecision::RejectedForFollowUp,
    ]
    .into_iter()
    .enumerate()
    {
        observation = jobs
            .review_investigation_finding(&ReviewInvestigationFindingRequest {
                schema_version: 1,
                subject: observation.subject.clone(),
                expected_revision: observation.revision,
                decision,
            })
            .unwrap();
        assert_eq!(observation.revision, revision as u64 + 1);
        assert_eq!(observation.decision, Some(decision));
        assert_eq!(
            jobs.investigation_finding_reviews(&fixture.id, &result.result_id)
                .unwrap()
                .reviews[0],
            observation
        );
        assert_eq!(
            jobs.investigation_result(&fixture.id).unwrap(),
            Some(result.clone())
        );
        assert_eq!(
            jobs.investigation_ledger(&fixture.id).unwrap(),
            Some(ledger.clone())
        );
        assert_eq!(jobs.investigation(&fixture.id).unwrap(), detail);
        assert_eq!(
            jobs.investigation_receipt_references(&ledger.receipt_references[0].source_id)
                .unwrap(),
            references
        );
        assert_eq!(
            serde_json::to_value(jobs.list().unwrap()).unwrap(),
            jobs_before
        );
        assert_eq!(durable_inputs(&fixture), durable_before);
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls_before);
        assert_eq!(
            state.primary_sources.evidence_access_attempts(),
            retained_before
        );
        assert_eq!(
            state.sources.lock().unwrap().checkout_access_attempts(),
            checkout_before
        );
        assert_eq!(*provider.prompts.lock().unwrap(), prompts_before);
        assert_eq!(*provider.copied.lock().unwrap(), copied_before);
    }
    drop(jobs);
    assert_eq!(
        state.graph.lock().unwrap().read_snapshot().unwrap(),
        graph_before
    );
    assert_eq!(
        serde_json::to_vec(
            &crate::export_spec_blocking(
                spec::ExportMode::BestEffort,
                &fixture.app.state::<AppState>(),
            )
            .unwrap()
        )
        .unwrap(),
        export_before
    );
    assert_eq!(
        state.primary_sources.evidence_access_attempts(),
        retained_before
    );
    assert_eq!(
        state.sources.lock().unwrap().checkout_access_attempts(),
        checkout_before
    );
    let historical_after =
        crate::investigations::history::read(&state, &fixture.id, &ledger, citation_id).unwrap();
    assert_eq!(historical_after, historical_before);
    assert_eq!(
        state.primary_sources.evidence_access_attempts(),
        retained_before + 1,
        "only this explicit historical inspection obtains another retained-source lease"
    );
    assert_eq!(
        state.sources.lock().unwrap().checkout_access_attempts(),
        checkout_before
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), calls_before);
    assert_eq!(durable_inputs(&fixture), durable_before);
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_result(&fixture.id).unwrap(),
        Some(result.clone())
    );
    assert_eq!(
        reopened
            .investigation_finding_reviews(&fixture.id, &result.result_id)
            .unwrap()
            .reviews[0],
        observation
    );
}

// AC-0296 / T-0296 / MT-W8B-04 step 3 synthetic host counterpart: unavailable
// retained bytes are not corrupt saved bindings and never select checkout bytes.
#[test]
fn finding_review_survives_confirmed_forget_and_retained_object_failures() {
    for (scenario, expected) in [
        ("confirmed_forget", InvestigationCitationStatus::Unavailable),
        ("missing_object", InvestigationCitationStatus::Unavailable),
        ("corrupt_object", InvestigationCitationStatus::Invalid),
    ] {
        let provider = Arc::new(Scripted::new(Mode::Finish));
        let fixture = cited_review_fixture(provider.as_ref());
        fixture.run(provider.clone());
        let state = fixture.app.state::<AppState>();
        let (result, ledger, detail, references, accepted) = {
            let mut jobs = state.jobs.lock().unwrap();
            let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
            let ledger = jobs.investigation_ledger(&fixture.id).unwrap().unwrap();
            let detail = jobs.investigation(&fixture.id).unwrap();
            let references = jobs
                .investigation_receipt_references(&ledger.receipt_references[0].source_id)
                .unwrap();
            let initial = jobs
                .investigation_finding_reviews(&fixture.id, &result.result_id)
                .unwrap();
            let accepted = jobs
                .review_investigation_finding(&ReviewInvestigationFindingRequest {
                    schema_version: 1,
                    subject: initial.reviews[0].subject.clone(),
                    expected_revision: 0,
                    decision: FindingReviewDecision::AcceptedForFollowUp,
                })
                .unwrap();
            (result, ledger, detail, references, accepted)
        };
        let citation_id = &result.findings[0].citation_ids[0];
        let original =
            crate::investigations::history::read(&state, &fixture.id, &ledger, citation_id)
                .unwrap();
        assert_eq!(original.status, InvestigationCitationStatus::Available);
        assert_eq!(original.text.as_deref(), Some("ok === false"));
        assert!(matches!(
            original.citation.origin,
            InvestigationEvidenceOrigin::CapturedPrimarySource { .. }
        ));
        let target = fixture._directory.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(
            target.join("rule.ts"),
            "export const currentCheckoutReplacement = true;\n",
        )
        .unwrap();
        let checkout_control = state.sources.lock().unwrap().checkout_access_attempts();
        let replacement = crate::source_access::with_registered_read(
            &state.sources,
            &original.citation.source.as_ref().unwrap().repo,
            |root| std::fs::read_to_string(root.join("rule.ts")).map_err(|e| e.to_string()),
        )
        .unwrap();
        assert!(replacement.contains("currentCheckoutReplacement"));
        assert_eq!(
            state.sources.lock().unwrap().checkout_access_attempts(),
            checkout_control + 1
        );
        let history_before = stored_rows(&fixture.app_data.join("state.db"));
        let receipts_before =
            stored_rows(&fixture.app_data.join("retained-source/receipts.sqlite"));
        let graph_before = state.graph.lock().unwrap().read_snapshot().unwrap();
        let export_before = serde_json::to_vec(
            &crate::export_spec_blocking(
                spec::ExportMode::BestEffort,
                &fixture.app.state::<AppState>(),
            )
            .unwrap(),
        )
        .unwrap();
        let source_id = &ledger.receipt_references[0].source_id;
        if scenario == "confirmed_forget" {
            let preview = crate::primary_source::preview(&state, source_id).unwrap();
            assert!(
                preview.investigation_references > 0,
                "confirmation includes this real saved receipt use"
            );
            assert!(preview.captures > 0 && preview.receipts > 0);
            assert_eq!(
                crate::primary_source::forget(&state, source_id, &preview.fingerprint).unwrap(),
                preview.captures as u64
            );
            let forgotten = crate::primary_source::preview(&state, source_id).unwrap();
            assert_eq!(forgotten.captures, 0);
            assert_eq!(
                forgotten.investigation_references,
                preview.investigation_references
            );
            assert_eq!(forgotten.receipts, preview.investigation_references);
        } else {
            let path = fixture.app_data.join("retained-source/captures.sqlite");
            assert!(path.is_file());
            let connection = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
            )
            .unwrap();
            let changed = connection
                .execute(
                    if scenario == "missing_object" {
                        "DELETE FROM objects"
                    } else {
                        "UPDATE objects SET bytes=X'00'"
                    },
                    [],
                )
                .unwrap();
            assert!(changed > 0, "fault must affect existing retained objects");
            assert_eq!(
                stored_rows(&fixture.app_data.join("retained-source/receipts.sqlite")),
                receipts_before
            );
        }
        // Forget may invalidate derived graph caches; it must preserve raw facts,
        // pinned receipt identities and the whole saved history outside review metadata.
        assert_eq!(
            stored_rows(&fixture.app_data.join("state.db")),
            history_before
        );
        assert_eq!(
            state.graph.lock().unwrap().read_snapshot().unwrap(),
            graph_before
        );
        assert_eq!(
            serde_json::to_vec(
                &crate::export_spec_blocking(
                    spec::ExportMode::BestEffort,
                    &fixture.app.state::<AppState>()
                )
                .unwrap()
            )
            .unwrap(),
            export_before
        );
        let durable_after_fault = durable_inputs(&fixture);
        let calls = provider.calls.load(Ordering::SeqCst);
        assert_eq!(calls, 3);
        let checkout = state.sources.lock().unwrap().checkout_access_attempts();
        let retained = state.primary_sources.evidence_access_attempts();
        let observed =
            crate::investigations::history::read(&state, &fixture.id, &ledger, citation_id)
                .unwrap();
        assert_eq!(observed.status, expected, "{scenario}");
        assert!(
            observed.text.is_none(),
            "{scenario} cannot fall back to replacement checkout"
        );
        assert_eq!(
            observed.citation, original.citation,
            "saved binding stays valid"
        );
        assert_eq!(
            state.primary_sources.evidence_access_attempts(),
            retained + 1
        );
        assert_eq!(
            state.sources.lock().unwrap().checkout_access_attempts(),
            checkout
        );
        let retained = state.primary_sources.evidence_access_attempts();
        let mut reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
        assert_eq!(
            reopened
                .investigation_finding_reviews(&fixture.id, &result.result_id)
                .unwrap()
                .reviews[0],
            accepted
        );
        assert_eq!(
            reopened.investigation_result(&fixture.id).unwrap(),
            Some(result.clone())
        );
        assert_eq!(
            reopened.investigation_ledger(&fixture.id).unwrap(),
            Some(ledger.clone())
        );
        assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
        assert_eq!(
            reopened
                .investigation_receipt_references(source_id)
                .unwrap(),
            references
        );
        let rejected = reopened
            .review_investigation_finding(&ReviewInvestigationFindingRequest {
                schema_version: 1,
                subject: accepted.subject.clone(),
                expected_revision: accepted.revision,
                decision: FindingReviewDecision::RejectedForFollowUp,
            })
            .unwrap();
        assert_eq!(rejected.revision, 2);
        assert_eq!(
            rejected.decision,
            Some(FindingReviewDecision::RejectedForFollowUp)
        );
        assert_eq!(
            reopened
                .investigation_finding_reviews(&fixture.id, &result.result_id)
                .unwrap()
                .reviews[0],
            rejected
        );
        assert_eq!(durable_inputs(&fixture), durable_after_fault);
        assert_eq!(state.primary_sources.evidence_access_attempts(), retained);
        assert_eq!(
            state.sources.lock().unwrap().checkout_access_attempts(),
            checkout
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
    }
}
