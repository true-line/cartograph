use super::*;
use llm::bounded::CompletionLimits;
use llm::{EgressFirewall, EgressPolicy, OllamaProvider};
use std::time::Duration;

fn input(specialist: SpecialistId) -> InvestigationInput {
    input_with_history(specialist, &[])
}

fn input_with_history(specialist: SpecialistId, history: &[String]) -> InvestigationInput {
    let mut basis = super::disclosure_tests::basis_with_source();
    let pages = vec![super::projection_tests::page(&basis, "Recovered behavior")];
    super::projection_tests::bind_pages(&mut basis, &pages);
    let source = "password=supersecret123";
    basis.citations[1].text_hash = Some(content_hash(source.as_bytes()));
    basis.citations[1].source.as_mut().unwrap().byte_end = source.len() as u64;
    if !history.is_empty() {
        let bytes = serde_json::to_vec(history).unwrap();
        basis.supplied_history_hash = Some(content_hash(&bytes));
        basis.supplied_history_bytes = bytes.len();
    }
    let evidence = crate::AgentEvidence {
        id: basis.citations[1].citation_id.clone(),
        source: basis.citations[1].source.clone().unwrap(),
        text: source.into(),
    };
    InvestigationInput::prepare(
        "Inspect the behavior",
        specialist,
        basis,
        &pages,
        &[evidence],
        history,
        &InvestigationUsage::default(),
    )
    .unwrap()
}

// AC-0235: a supplied history marker requires an actual non-null representation.
#[test]
fn compact_disclosure_sealing_rejects_missing_or_null_history() {
    let history = vec![
        json!({
            "parent_investigation_id": "inv-parent",
            "original_graph_snapshot_id": null,
            "original_scope": {"type": "all"},
            "saved_execution_status": "completed",
            "cancellation_requested": false,
            "invocation_outcome_pending": false,
            "authority": "Prior findings remain T3/InferredWeak.",
            "saved_result": null
        })
        .to_string(),
    ];
    let input = input_with_history(SpecialistId::DomainAnalystV3, &history);
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let action = input.action("investigation-1:step-1".into()).unwrap();
    let prepared = firewall
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    assert!(
        input
            .seal_disclosure(&prepared)
            .unwrap()
            .unwrap()
            .history
            .is_some()
    );
    for null_history in [false, true] {
        let mut action = input.action("investigation-1:step-1".into()).unwrap();
        let mut prompt: serde_json::Value = serde_json::from_str(&action.payload.prompt).unwrap();
        let fields = prompt.as_object_mut().unwrap();
        if null_history {
            fields.insert(
                "prior_findings_as_t3_history".into(),
                serde_json::Value::Null,
            );
        } else {
            fields.remove("prior_findings_as_t3_history");
        }
        action.payload.prompt = prompt.to_string();
        let prepared = firewall
            .prepare_bounded(&provider, &action, &CompletionLimits::default())
            .unwrap();
        // Reach the sealing check itself, independently of action matching.
        assert_eq!(
            super::sealing::seal(input.ledger(), &action.payload, &prepared),
            Err(InvestigationError::InvalidInput)
        );
    }
}

// AC-0235: binding is to actual firewall-redacted input, not acquired source.
#[test]
fn compact_disclosure_seals_actual_redacted_payload_and_admits_versioned_result() {
    let input = input(SpecialistId::DomainAnalystV3);
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let action = input.action("investigation-1:step-1".into()).unwrap();
    let prepared = firewall
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    assert!(prepared.matches_action(&action));
    let disclosure = input.seal_disclosure(&prepared).unwrap().unwrap();
    assert_eq!(
        disclosure.payload_hash,
        prepared.preview().egress.payload_hash
    );
    assert!(disclosure.sources[0].redacted);
    assert_eq!(
        disclosure.sources[0].disclosed_text_hash.as_ref().unwrap(),
        &content_hash(prepared.preview().egress.payload.spans[0].text.as_bytes())
    );
    assert_eq!(
        disclosure.eligible_citations(input.ledger()).unwrap().len(),
        2
    );
    let result = input
        .admit_result_for_step(
            "investigation-1",
            "step-1",
            Some(&disclosure),
            &super::action(),
            None,
            "2026-10-19T21:00:00Z".into(),
        )
        .unwrap();
    assert_eq!(result.schema_version, 2);
    result
        .validate_with_disclosure(input.ledger(), "step-1", &disclosure)
        .unwrap();
    assert!(
        input
            .admit_result("investigation-1", &super::action(), None, "now".into())
            .is_err()
    );
    assert!(
        input
            .admit_result_for_step(
                "investigation-1",
                "step-1",
                None,
                &super::action(),
                None,
                "now".into()
            )
            .is_err()
    );
}

#[test]
fn compact_disclosure_rejects_another_firewall_preparation_with_same_action_id() {
    let input = input(SpecialistId::DomainAnalystV3);
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let mut action = input.action("investigation-1:step-1".into()).unwrap();
    action.payload.spans[0].text = "unrelated supplied source".into();
    let prepared = firewall
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    assert!(input.seal_disclosure(&prepared).is_err());
}

#[test]
fn compact_disclosure_keeps_legacy_prompt_and_result_paths_available() {
    let legacy = input(SpecialistId::DomainAnalyst);
    let compact = input(SpecialistId::DomainAnalystV3);
    let old = legacy.action("legacy:step-1".into()).unwrap();
    let new = compact.action("compact:step-1".into()).unwrap();
    let old_prompt: serde_json::Value = serde_json::from_str(&old.payload.prompt).unwrap();
    let new_prompt: serde_json::Value = serde_json::from_str(&new.payload.prompt).unwrap();
    assert_eq!(old_prompt["schema_version"], 1);
    assert!(old_prompt.get("input_ledger").is_some());
    assert_eq!(new_prompt["schema_version"], 2);
    assert!(new_prompt.get("input_ledger").is_none());
    assert!(new_prompt.get("compact_context").is_some());
    assert_eq!(old.payload.spans, new.payload.spans);
    assert_eq!(legacy.ledger(), compact.ledger());
    assert_eq!(
        legacy
            .admit_result("legacy", &super::action(), None, "now".into())
            .unwrap()
            .schema_version,
        1
    );
}

// AC-0235: the visible opaque reacquisition handle is not supplied evidence.
#[test]
fn compact_disclosure_omitted_source_handle_cannot_support_a_finding() {
    let input = input(SpecialistId::EvidenceAuditorV3)
        .select_sources(&BTreeSet::new(), DisclosureOmissionReason::WorkingSetBudget)
        .unwrap();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let action = input.action("investigation-1:step-2".into()).unwrap();
    assert!(action.payload.spans.is_empty());
    let prompt: serde_json::Value = serde_json::from_str(&action.payload.prompt).unwrap();
    assert_eq!(
        prompt["omitted_sources"][0]["handle"]["citation_id"],
        "evidence-1"
    );
    assert_eq!(prompt["omitted_sources"][0]["handle"]["role"], "provenance");
    assert_eq!(prompt["omitted_sources"][0]["handle"]["index"], 0);
    let prepared = firewall
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    let disclosure = input.seal_disclosure(&prepared).unwrap().unwrap();
    assert_eq!(
        disclosure.sources[0].representation,
        DisclosureRepresentation::Omitted {
            reason: DisclosureOmissionReason::WorkingSetBudget,
        }
    );
    assert!(
        !disclosure
            .eligible_citations(input.ledger())
            .unwrap()
            .contains("evidence-1")
    );
    let mut finish = super::action();
    if let InvestigationAction::Finish { findings, .. } = &mut finish {
        findings[0].citation_ids = vec!["evidence-1".into()];
    }
    assert!(matches!(
        input.admit_result_for_step(
            "investigation-1",
            "step-2",
            Some(&disclosure),
            &finish,
            None,
            "now".into()
        ),
        Err(InvestigationError::InvalidCitation)
    ));
}

// AC-0236: actual serialized bytes on the same acquisition, never byte/token estimates.
#[test]
fn compact_disclosure_matched_payload_measurement_keeps_unobserved_metrics_unknown() {
    let mut basis = ledger();
    let raw =
        super::projection_tests::page(&basis, &"Recovered deterministic behavior. ".repeat(100));
    let pages = vec![raw.clone(), raw.clone(), raw];
    super::projection_tests::bind_pages(&mut basis, &pages);
    let prepare = |specialist| {
        InvestigationInput::prepare(
            "Inspect the behavior",
            specialist,
            basis.clone(),
            &pages,
            &[],
            &[],
            &InvestigationUsage::default(),
        )
        .unwrap()
    };
    let baseline = prepare(SpecialistId::DomainAnalyst);
    let compact = prepare(SpecialistId::DomainAnalystV3);
    assert_eq!(baseline.ledger(), compact.ledger());
    let bytes = |input: &InvestigationInput| {
        serde_json::to_vec(&input.action("measurement:step-1".into()).unwrap().payload)
            .unwrap()
            .len()
    };
    let (baseline_bytes, compact_bytes) = (bytes(&baseline), bytes(&compact));
    assert!(compact_bytes < baseline_bytes);
    let measurement = json!({
        "acquisition_hash": basis.fingerprint().unwrap(),
        "baseline": {"payload_bytes": baseline_bytes, "reported_input_tokens": null,
            "reported_output_tokens": null, "provider_calls": null, "provider_latency_ms": null},
        "compact": {"payload_bytes": compact_bytes, "reported_input_tokens": null,
            "reported_output_tokens": null, "provider_calls": null, "provider_latency_ms": null},
        "method": "serialized CompletionPayload; no provider executed"
    });
    println!("{measurement}");
    assert!(measurement["compact"]["reported_input_tokens"].is_null());
}
