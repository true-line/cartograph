use super::*;
use crate::TaskRangeRole;
use crate::investigation::{
    INVESTIGATION_V1_POLICY, InvestigationCitation, InvestigationEvidenceOrigin,
    InvestigationInputLedger, policy::INVESTIGATION_V2_POLICY,
};

fn ledger() -> InvestigationInputLedger {
    crate::investigation::tests::ledger()
}

fn source(ledger: &InvestigationInputLedger, index: usize, bytes: usize) -> InvestigationCitation {
    let mut citation = ledger.citations[0].clone();
    citation.citation_id = format!("source-{index}");
    citation.source = Some(core_prov::EvidenceRef {
        repo: "example/app".into(),
        path: "src/lib.rs".into(),
        commit_sha: "a".repeat(40),
        byte_start: 0,
        byte_end: bytes as u64,
    });
    citation.role = Some(TaskRangeRole::Provenance);
    citation.index = Some(index as u32);
    citation.text_hash = Some("a".repeat(64));
    citation.origin = InvestigationEvidenceOrigin::WorkingTreeUnverified;
    citation
}

#[test]
fn ledger_policy_preserves_legacy_wire_and_qualified_fingerprints() {
    // AC-0188: policy admission does not enter a legacy ledger's hash encoding.
    let legacy = ledger();
    assert_eq!(
        legacy.validate(),
        legacy.validate_with_policy(&INVESTIGATION_V1_POLICY)
    );
    let wire = serde_json::to_vec(&legacy).unwrap();
    let mut expected = b"cartograph:investigation-input-ledger:v1\0".to_vec();
    expected.extend(&wire);
    assert_eq!(
        legacy.fingerprint().unwrap(),
        core_prov::content_hash(&expected)
    );
    assert_eq!(
        legacy.fingerprint().unwrap(),
        legacy
            .fingerprint_with_policy(&INVESTIGATION_V1_POLICY)
            .unwrap()
    );
    assert_eq!(
        legacy.fingerprint().unwrap(),
        legacy
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap()
    );
    let (archive, _) = super::tests::fixture();
    let mut qualified = legacy;
    qualified.schema_version = 2;
    qualified.scope_snapshot_id = archive.proof().snapshot_id.clone();
    qualified.graph_snapshot_id = qualified.scope_snapshot_id.clone();
    qualified.coverage = Some(archive.proof().clone());
    assert_eq!(
        qualified.fingerprint().unwrap(),
        qualified
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap()
    );
    let mut expected = b"cartograph:investigation-input-ledger:v2\0".to_vec();
    expected.extend(serde_json::to_vec(&qualified).unwrap());
    assert_eq!(
        qualified.fingerprint().unwrap(),
        core_prov::content_hash(&expected)
    );
}

#[test]
fn ledger_policy_enforces_selected_fact_and_combined_citation_capacity() {
    // AC-0188: all selected graph citations and source items share one ledger.
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        let limits = policy.limits();
        let mut value = ledger();
        let selected = value.selected_facts[0].clone();
        let graph = value.citations[0].clone();
        value.selected_facts.clear();
        value.citations.clear();
        for index in 0..limits.selected_facts {
            let mut selected = selected.clone();
            selected.fact = crate::TaskFactKey::Node {
                id: format!("node:policy-{index:03}"),
            };
            let mut citation = graph.clone();
            citation.citation_id = format!("graph-{index}");
            citation.fact = selected.fact.clone();
            value.selected_facts.push(selected);
            value.citations.push(citation);
        }
        let mut query = value.queries[0].clone();
        query.query.max_facts = 32;
        query.returned_facts = 32;
        query.total_selected = limits.selected_facts;
        value.queries = vec![query; 2];
        for index in 0..limits.evidence_items {
            value.citations.push(source(&value, index, 1));
        }
        value.validate_with_policy(policy).unwrap();
        value.fingerprint_with_policy(policy).unwrap();
        value
            .citations
            .push(source(&value, limits.evidence_items, 1));
        assert_eq!(
            value.validate_with_policy(policy),
            Err(InvestigationError::InvalidInput)
        );
        value.citations.pop();
        value.selected_facts.push(selected);
        assert_eq!(
            value.validate_with_policy(policy),
            Err(InvestigationError::InvalidInput)
        );
    }
}

#[test]
fn ledger_policy_enforces_query_and_combined_read_boundaries() {
    // AC-0188: the trusted policy reaches both the direct and aggregate paths.
    let (archive, request) = super::tests::fixture();
    let raw = serde_json::to_vec(&archive.read(&request).unwrap()).unwrap();
    let read = InvestigationCoverageReadManifest {
        request,
        response_hash: core_prov::content_hash(&raw),
        response_bytes: raw.len(),
    };
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        let limit = policy.limits().tool_actions as usize;
        let mut queries = ledger();
        queries.queries = vec![queries.queries[0].clone(); limit];
        queries.validate_with_policy(policy).unwrap();
        queries.fingerprint_with_policy(policy).unwrap();
        queries.queries.push(queries.queries[0].clone());
        assert_eq!(
            queries.validate_with_policy(policy),
            Err(InvestigationError::InvalidInput)
        );
        let mut mixed = ledger();
        mixed.schema_version = 2;
        mixed.graph_snapshot_id = archive.proof().snapshot_id.clone();
        mixed.scope_snapshot_id = mixed.graph_snapshot_id.clone();
        mixed.coverage = Some(archive.proof().clone());
        mixed.coverage_reads = vec![read.clone(); limit - 1];
        mixed.validate_with_policy(policy).unwrap();
        mixed.coverage_reads.push(read.clone());
        assert_eq!(
            mixed.validate_with_policy(policy),
            Err(InvestigationError::LimitExceeded)
        );
        archive
            .validate_reads_with_policy(&vec![read.clone(); limit], policy)
            .unwrap();
        assert_eq!(
            archive.validate_reads_with_policy(&vec![read.clone(); limit + 1], policy),
            Err(InvestigationError::LimitExceeded)
        );
    }
    let mut larger = ledger();
    larger.queries = vec![larger.queries[0].clone(); 9];
    assert!(larger.validate().is_err());
    larger
        .validate_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let mut forged = read;
    forged.response_hash = "0".repeat(64);
    assert_eq!(
        archive.validate_reads_with_policy(&[forged], &INVESTIGATION_V2_POLICY),
        Err(InvestigationError::InvalidInput)
    );
}

#[test]
fn ledger_policy_enforces_evidence_items_and_aggregate_source_bytes() {
    // AC-0188: source item and byte allowances are independent policy fields.
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        let limits = policy.limits();
        let mut items = ledger();
        for index in 0..limits.evidence_items {
            items.citations.push(source(&items, index, 1));
        }
        items.validate_with_policy(policy).unwrap();
        items.fingerprint_with_policy(policy).unwrap();
        items
            .citations
            .push(source(&items, limits.evidence_items, 1));
        assert_eq!(
            items.validate_with_policy(policy),
            Err(InvestigationError::InvalidInput)
        );
        let mut bytes = ledger();
        let count = limits.evidence_bytes / 8192;
        for index in 0..count {
            bytes.citations.push(source(&bytes, index, 8192));
        }
        bytes.validate_with_policy(policy).unwrap();
        bytes.citations.push(source(&bytes, count, 1));
        assert_eq!(
            bytes.validate_with_policy(policy),
            Err(InvestigationError::InvalidInput)
        );
    }
    let mut larger = ledger();
    for index in 0..13 {
        larger.citations.push(source(&larger, index, 1));
    }
    assert!(larger.validate().is_err());
    larger
        .validate_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
}

#[test]
fn ledger_policy_retains_captured_validation_byte_ceiling() {
    // AC-0188: greater call/item allowances never raise captured-file admission.
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        let (_, mut value, _) = crate::investigation::tests::inspection_tests::fixture();
        let mut captured_source = value.citations[1].clone();
        if let InvestigationEvidenceOrigin::CapturedPrimarySource { captured, .. } =
            &mut captured_source.origin
        {
            captured.file.byte_len = 16 * 1024 * 1024;
        }
        value.citations.truncate(1);
        for index in 0..8 {
            let mut citation = captured_source.clone();
            citation.citation_id = format!("capture-{index}");
            value.citations.push(citation);
        }
        value.validate_with_policy(policy).unwrap();
        captured_source.citation_id = "capture-extra".into();
        value.citations.push(captured_source);
        assert_eq!(
            value.validate_with_policy(policy),
            Err(InvestigationError::InvalidInput)
        );
    }
}

#[test]
fn result_policy_reaches_disclosure_and_saved_result_validation() {
    // AC-0188: an accepted larger ledger must not fall back to v1 downstream.
    use crate::investigation::*;
    let (mut value, mut disclosure, action) =
        crate::investigation::result_disclosure_tests::basis();
    value.queries = vec![value.queries[0].clone(); 9];
    disclosure.queries = (0..9)
        .map(|query_index| {
            let mut query = disclosure.queries[0].clone();
            query.query_index = query_index;
            query
        })
        .collect();
    disclosure.input_ledger_hash = value
        .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    assert!(
        InvestigationResult::admit_with_disclosure(
            "policy-result",
            &value,
            "policy-step",
            &disclosure,
            &action,
            std::iter::empty(),
            None,
            "2026-10-21T12:00:00Z".into(),
        )
        .is_err()
    );
    let result = InvestigationResult::admit_with_disclosure_and_policy(
        "policy-result",
        &value,
        "policy-step",
        &disclosure,
        &action,
        std::iter::empty(),
        None,
        "2026-10-21T12:00:00Z".into(),
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    result
        .validate_with_disclosure_and_policy(
            &value,
            "policy-step",
            &disclosure,
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
    assert!(
        result
            .validate_with_disclosure(&value, "policy-step", &disclosure)
            .is_err()
    );
    assert_eq!(
        result.input_ledger_hash,
        value
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap()
    );
    disclosure.payload_hash = "0".repeat(64);
    assert!(
        result
            .validate_with_disclosure_and_policy(
                &value,
                "policy-step",
                &disclosure,
                &INVESTIGATION_V2_POLICY
            )
            .is_err()
    );
}

#[test]
fn result_policy_reaches_extended_inspection_assessment() {
    // AC-0188: normalization, requirements identity, assessment and reopen use
    // one trusted policy; individual target identities retain their old domain.
    use crate::investigation::*;
    let (target, value, mut disclosure) = crate::investigation::tests::inspection_tests::fixture();
    let targets = (0..13)
        .map(|index| {
            let mut target = target.clone();
            target.index = index;
            target.target_id = target.content_id().unwrap();
            target
        })
        .collect::<Vec<_>>();
    assert!(normalize_inspection_targets(&targets).is_err());
    disclosure.inspection_requirements_hash =
        Some(inspection_requirements_hash_with_policy(&targets, &INVESTIGATION_V2_POLICY).unwrap());
    let action = InvestigationAction::Finish {
        findings: vec![],
        knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
        limitations: vec!["The selected evidence is insufficient.".into()],
    };
    let result = InvestigationResult::admit_with_inspection_and_policy(
        "policy-inspection",
        &value,
        "policy-step",
        &disclosure,
        &action,
        std::iter::empty(),
        None,
        "2026-10-21T12:00:00Z".into(),
        &targets,
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    assert_eq!(
        result
            .inspection_assessment
            .as_ref()
            .unwrap()
            .requirements
            .len(),
        13
    );
    result
        .validate_with_disclosure_and_policy(
            &value,
            "policy-step",
            &disclosure,
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
    assert!(
        result
            .validate_with_disclosure(&value, "policy-step", &disclosure)
            .is_err()
    );
}
