use super::*;
use crate::{
    InputClosureStatus, PrimarySourceScope, TaskCaptureFileRef, TaskCaptureSpanRef,
    TaskSourceAssociation,
};

pub(in crate::investigation) fn fixture() -> (
    InvestigationInspectionTarget,
    InvestigationInputLedger,
    InvestigationDisclosure,
) {
    let mut ledger = super::disclosure_tests::basis_with_source();
    let receipt = format!("ts-primary-v2:{}", "c".repeat(64));
    let source_id = format!("src_{}", "d".repeat(32));
    let source = ledger.citations[1].source.clone().unwrap();
    let captured = TaskCaptureSpanRef {
        file: TaskCaptureFileRef {
            source_id: source_id.clone(),
            capture_id: format!("capture-v1:{}", "e".repeat(64)),
            path: source.path.clone(),
            digest: content_hash(b"private text"),
            byte_len: 12,
        },
        byte_start: 0,
        byte_end: 12,
    };
    ledger.selected_facts[0].binding = Some(TaskSourceAssociation {
        repo_key: source.repo.clone(),
        receipt_id: receipt.clone(),
        emitted_fact_digest: ledger.selected_facts[0].fact_digest.clone(),
    });
    ledger.receipt_references = vec![InvestigationReceiptReference {
        source_id: source_id.clone(),
        repo_key: source.repo.clone(),
        receipt_id: receipt.clone(),
    }];
    ledger.citations[1].origin = InvestigationEvidenceOrigin::CapturedPrimarySource {
        registered_source_id: source_id.clone(),
        receipt_id: receipt.clone(),
        receipt_inventory_index: 3,
        captured: captured.clone(),
        scope: PrimarySourceScope::PrimarySourceOnly,
        input_closure: InputClosureStatus::InputClosureNotEstablished,
    };
    let mut target = InvestigationInspectionTarget {
        schema_version: 1,
        target_id: String::new(),
        graph_snapshot_id: ledger.graph_snapshot_id.clone(),
        fact: ledger.citations[1].fact.clone(),
        fact_digest: ledger.citations[1].fact_digest.clone(),
        registered_source_id: source_id,
        receipt_id: receipt,
        receipt_inventory_index: 3,
        role: ledger.citations[1].role.unwrap(),
        index: 0,
        source,
        captured,
    };
    target.target_id = target.content_id().unwrap();
    target.validate().unwrap();
    ledger.validate().unwrap();
    let supplied = || DisclosureRepresentation::Supplied {
        representation_hash: content_hash(b"representation"),
    };
    let disclosure = InvestigationDisclosure {
        admission: None,
        query_history: None,
        completion_contract_hash: None,
        completion_metadata_hash: None,
        coverage_details: vec![],
        artifact_pages: vec![],
        coverage: None,
        schema_version: 2,
        policy: COMPACT_DISCLOSURE_POLICY.into(),
        inspection_requirements_hash: Some(
            inspection_requirements_hash(std::slice::from_ref(&target)).unwrap(),
        ),
        input_ledger_hash: ledger.fingerprint().unwrap(),
        graph_snapshot_id: ledger.graph_snapshot_id.clone(),
        scope_snapshot_id: ledger.scope_snapshot_id.clone(),
        payload_hash: content_hash(b"payload"),
        facts: vec![DisclosedInvestigationFact {
            fact: target.fact.clone(),
            representation: supplied(),
        }],
        queries: vec![DisclosedInvestigationQuery {
            query_index: 0,
            representation: supplied(),
        }],
        sources: vec![DisclosedInvestigationSource {
            citation_id: "evidence-1".into(),
            representation: supplied(),
            disclosed_text_hash: ledger.citations[1].text_hash.clone(),
            redacted: false,
        }],
        history: None,
    };
    disclosure.validate(&ledger).unwrap();
    (target, ledger, disclosure)
}
fn admitted_result(
    target: &InvestigationInspectionTarget,
    ledger: &InvestigationInputLedger,
    disclosure: &InvestigationDisclosure,
    cited: bool,
) -> InvestigationResult {
    let mut action = action();
    if cited && let InvestigationAction::Finish { findings, .. } = &mut action {
        findings[0].citation_ids = vec!["evidence-1".into()];
    }
    InvestigationResult::admit_with_inspection(
        "inv-one",
        ledger,
        "step-1",
        disclosure,
        &action,
        std::iter::empty(),
        None,
        "2026-10-20T00:00:00Z".into(),
        std::slice::from_ref(target),
    )
    .unwrap()
}
// AC-0261: supplied-only exact observation is independent of citation eligibility.
#[test]
fn inspection_obligations_distinguish_acquired_supplied_cited_redacted_and_omitted() {
    let (target, ledger, disclosure) = fixture();
    for cited in [false, true] {
        let result = admitted_result(&target, &ledger, &disclosure, cited);
        assert_eq!(result.schema_version, 3);
        let row = &result.inspection_assessment.as_ref().unwrap().requirements[0];
        assert!(row.acquired && row.supplied);
        assert_eq!(row.cited, cited);
        result
            .validate_with_disclosure(&ledger, "step-1", &disclosure)
            .unwrap();
    }
    let mut redacted = disclosure.clone();
    redacted.sources[0].redacted = true;
    redacted.sources[0].disclosed_text_hash = Some(content_hash(b"transformed"));
    assert!(
        redacted
            .eligible_citations(&ledger)
            .unwrap()
            .contains("evidence-1")
    );
    let result = admitted_result(&target, &ledger, &redacted, true);
    let row = &result.inspection_assessment.as_ref().unwrap().requirements[0];
    assert!(row.acquired && row.cited && !row.supplied);
    assert_eq!(row.status, InvestigationInspectionStatus::Unmet);
    let mut omitted = disclosure.clone();
    omitted.sources[0].representation = DisclosureRepresentation::Omitted {
        reason: DisclosureOmissionReason::WorkingSetBudget,
    };
    omitted.sources[0].disclosed_text_hash = None;
    let result = admitted_result(&target, &ledger, &omitted, false);
    assert!(!result.inspection_assessment.unwrap().requirements[0].supplied);
}
#[test]
fn inspection_partial_finish_with_no_matching_read_remains_valid_unmet() {
    let (target, mut ledger, mut disclosure) = fixture();
    ledger.citations.pop();
    disclosure.sources.clear();
    disclosure.input_ledger_hash = ledger.fingerprint().unwrap();
    let result = admitted_result(&target, &ledger, &disclosure, false);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(
        result.inspection_assessment.unwrap().requirements[0].status,
        InvestigationInspectionStatus::Unmet
    );
}
#[test]
fn inspection_descriptor_and_assessment_bounds_precede_owned_copies() {
    let (target, ledger, disclosure) = fixture();
    let mut huge = target.clone();
    huge.target_id = "x".repeat(128 * 1024);
    assert!(huge.content_id().is_err());
    assert!(huge.validate().is_err());
    let mut huge = target.clone();
    huge.source.path = "x".repeat(128 * 1024);
    assert!(normalize_inspection_targets(&[huge]).is_err());
    assert_eq!(
        normalize_inspection_targets(&[target.clone(), target.clone()]).unwrap(),
        vec![target.clone()]
    );
    let mut changed = target.clone();
    changed.receipt_inventory_index += 1;
    changed.target_id = changed.content_id().unwrap();
    assert!(normalize_inspection_targets(&[target.clone(), changed]).is_err());
    let mut assessment = admitted_result(&target, &ledger, &disclosure, false)
        .inspection_assessment
        .unwrap();
    assessment.requirements = vec![assessment.requirements[0].clone(); 13];
    assert!(
        assessment
            .validate(&ledger, "step-1", &disclosure, &[])
            .is_err()
    );
}
#[test]
fn inspection_new_request_is_explicit_bounded_and_nonce_identity_is_canonical() {
    let (target, _, _) = fixture();
    let mut request = request();
    request.schema_version = 2;
    request.specialist_id = SpecialistId::EvidenceAuditorV5;
    request.expected_graph_revision = Some(target.graph_snapshot_id.clone());
    request.inspection_requirements = Some(vec![target.clone(), target.clone()]);
    request.validate().unwrap();
    let hash = request.intent_hash().unwrap();
    request.inspection_requirements = Some(vec![target]);
    assert_eq!(request.intent_hash().unwrap(), hash);
    request.specialist_id = SpecialistId::DomainAnalystV4;
    assert!(request.validate().is_err());
    request.inspection_requirements = None;
    request.schema_version = 1;
    request.validate().unwrap();
    assert!(
        !serde_json::to_string(&request)
            .unwrap()
            .contains("inspection_requirements")
    );
    assert_eq!(
        SpecialistId::EvidenceAuditorV5
            .definition()
            .unwrap()
            .version,
        5
    );
}

#[test]
fn inspection_request_preserves_absent_legacy_field_and_rejects_present_null() {
    let legacy = serde_json::to_value(request()).unwrap();
    let decoded: StartInvestigationRequest = serde_json::from_value(legacy.clone()).unwrap();
    decoded.validate().unwrap();
    let mut null = legacy;
    null["inspection_requirements"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<StartInvestigationRequest>(null).is_err());
}

// AC-0261: a missing original or disclosed hash is invalid, never exact supply.
#[test]
fn inspection_missing_text_hashes_fail_closed_before_assessment() {
    let (target, mut ledger, mut disclosure) = fixture();
    disclosure.sources[0].disclosed_text_hash = None;
    assert!(disclosure.validate(&ledger).is_err());
    assert!(
        InvestigationInspectionAssessment::derive(
            std::slice::from_ref(&target),
            &ledger,
            "step-1",
            &disclosure,
            &[]
        )
        .is_err()
    );
    ledger.citations[1].text_hash = None;
    assert!(
        ledger.validate().is_err(),
        "captured citations require original text hashes"
    );
    assert!(
        InvestigationInspectionAssessment::derive(
            std::slice::from_ref(&target),
            &ledger,
            "step-1",
            &disclosure,
            &[]
        )
        .is_err(),
        "None/None cannot form an admitted assessment"
    );
}

#[test]
fn inspection_finished_assessment_rejects_nonassessment_statuses() {
    let (target, ledger, disclosure) = fixture();
    let mut assessment = admitted_result(&target, &ledger, &disclosure, false)
        .inspection_assessment
        .unwrap();
    for status in [
        InvestigationInspectionStatus::Pending,
        InvestigationInspectionStatus::NotAssessed,
    ] {
        assessment.requirements[0].status = status;
        assert!(
            assessment
                .validate(&ledger, "step-1", &disclosure, &[])
                .is_err()
        );
    }
}

#[test]
fn inspection_twelve_realistic_targets_still_obey_aggregate_request_bytes() {
    let (target, _, _) = fixture();
    let targets: Vec<_> = (0..12)
        .map(|index| {
            let mut target = target.clone();
            target.fact = crate::TaskFactKey::Node {
                id: format!("symbol:src/services/checkout/eligibility-{index}.ts:enabled"),
            };
            target.source.path = format!("src/services/checkout/eligibility-{index}.ts");
            target.captured.file.path = target.source.path.clone();
            target.target_id = target.content_id().unwrap();
            target.validate().unwrap();
            target
        })
        .collect();
    assert_eq!(targets.len(), 12);
    assert!(serde_json::to_vec(&targets).unwrap().len() > 8192);
    assert!(normalize_inspection_targets(&targets).is_err());
    let mut request = request();
    request.schema_version = 2;
    request.specialist_id = SpecialistId::DomainAnalystV5;
    request.expected_graph_revision = Some(target.graph_snapshot_id);
    request.inspection_requirements = Some(targets);
    assert!(
        request.validate().is_err(),
        "the full 8 KiB request cap also applies below the count ceiling"
    );
}
