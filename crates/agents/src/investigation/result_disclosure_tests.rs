use super::*;
use crate::{TaskFactKey, TaskFactSelection, TaskRangeRole};
use core_prov::{ConfidenceTier, EvidenceRef, Tier, content_hash};

pub(in crate::investigation) fn basis() -> (
    InvestigationInputLedger,
    InvestigationDisclosure,
    InvestigationAction,
) {
    let fact = TaskFactKey::Node {
        id: "node:checkout".into(),
    };
    let digest = format!("node-v1:{}", "a".repeat(64));
    let snapshot = format!("context-v1:{}", "b".repeat(64));
    let ledger = InvestigationInputLedger {
        coverage_reads: vec![],
        artifact_reads: vec![],
        schema_version: 1,
        coverage: None,
        graph_snapshot_id: snapshot.clone(),
        scope_snapshot_id: snapshot.clone(),
        revision: 1,
        selected_facts: vec![TaskFactSelection {
            fact: fact.clone(),
            fact_digest: digest.clone(),
            binding: None,
        }],
        receipt_references: vec![],
        citations: vec![
            InvestigationCitation {
                citation_id: "fact-1".into(),
                fact: fact.clone(),
                fact_digest: digest.clone(),
                source: None,
                role: None,
                index: None,
                text_hash: None,
                origin: InvestigationEvidenceOrigin::GraphMetadata,
            },
            InvestigationCitation {
                citation_id: "source-1".into(),
                fact: fact.clone(),
                fact_digest: digest,
                source: Some(EvidenceRef {
                    repo: "example/app".into(),
                    path: "app.ts".into(),
                    commit_sha: "pinned-revision".into(),
                    byte_start: 0,
                    byte_end: 24,
                }),
                role: Some(TaskRangeRole::RuleExit),
                index: Some(0),
                text_hash: Some(content_hash(b"original acquired source")),
                origin: InvestigationEvidenceOrigin::WorkingTreeUnverified,
            },
        ],
        queries: vec![InvestigationQueryManifest {
            query: InvestigationQuery {
                scope: InvestigationScope::All,
                kind: None,
                labels: vec![],
                max_facts: 12,
                max_bytes: 16384,
                cursor: None,
            },
            response_hash: content_hash(b"query"),
            response_bytes: 200,
            returned_facts: 1,
            total_selected: 1,
            has_more: false,
        }],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    let supplied = DisclosureRepresentation::Supplied {
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
        inspection_requirements_hash: None,
        schema_version: 1,
        policy: COMPACT_DISCLOSURE_POLICY.into(),
        input_ledger_hash: ledger.fingerprint().unwrap(),
        graph_snapshot_id: snapshot.clone(),
        scope_snapshot_id: snapshot,
        payload_hash: content_hash(b"payload"),
        facts: vec![DisclosedInvestigationFact {
            fact,
            representation: supplied.clone(),
        }],
        queries: vec![DisclosedInvestigationQuery {
            query_index: 0,
            representation: supplied.clone(),
        }],
        sources: vec![DisclosedInvestigationSource {
            citation_id: "source-1".into(),
            representation: supplied,
            disclosed_text_hash: Some(content_hash(b"original acquired source")),
            redacted: false,
        }],
        history: None,
    };
    let action = InvestigationAction::Finish {
        findings: vec![ProposedInvestigationFinding {
            claim_kind: InvestigationClaimKind::ImplementedBehavior,
            title: "Observed guard".into(),
            statement: "The selected transition contains a rejection branch.".into(),
            citation_ids: vec!["source-1".into()],
            limitations: vec!["Other transitions remain unassessed.".into()],
        }],
        knowledge_completeness: KnowledgeCompleteness::Partial,
        limitations: vec!["Only selected evidence was assessed.".into()],
    };
    (ledger, disclosure, action)
}

fn admit(
    ledger: &InvestigationInputLedger,
    disclosure: &InvestigationDisclosure,
    action: &InvestigationAction,
) -> Result<InvestigationResult, InvestigationError> {
    InvestigationResult::admit_with_disclosure(
        "investigation-1",
        ledger,
        "step-2",
        disclosure,
        action,
        std::iter::empty(),
        None,
        "2026-10-19T00:00:00Z".into(),
    )
}

#[test]
fn v2_results_require_exact_saved_step_disclosure_and_disclosed_citations() {
    // AC-0235: acquired source cannot support a result when omitted from this call.
    let (ledger, disclosure, action) = basis();
    let result = admit(&ledger, &disclosure, &action).unwrap();
    assert_eq!(result.schema_version, 2);
    assert!(result.result_id.starts_with("investigation-result-v2:"));
    assert!(result.validate(&ledger).is_err());
    result
        .validate_with_disclosure(&ledger, "step-2", &disclosure)
        .unwrap();
    assert!(
        result
            .validate_with_disclosure(&ledger, "step-3", &disclosure)
            .is_err()
    );
    let mut omitted = disclosure.clone();
    omitted.sources[0].representation = DisclosureRepresentation::Omitted {
        reason: DisclosureOmissionReason::NotSelected,
    };
    omitted.sources[0].disclosed_text_hash = None;
    assert!(matches!(
        admit(&ledger, &omitted, &action),
        Err(InvestigationError::InvalidCitation)
    ));
    assert!(
        result
            .validate_with_disclosure(&ledger, "step-2", &omitted)
            .is_err()
    );
    let mut changed = disclosure.clone();
    changed.payload_hash = content_hash(b"different invocation");
    assert!(
        result
            .validate_with_disclosure(&ledger, "step-2", &changed)
            .is_err()
    );
    let mut stronger = result.clone();
    stronger.findings[0].tier = Tier::Deterministic;
    stronger.findings[0].confidence_tier = ConfidenceTier::Confirmed;
    assert!(
        stronger
            .validate_with_disclosure(&ledger, "step-2", &disclosure)
            .is_err()
    );
}

#[test]
fn v1_result_wire_and_fingerprint_remain_literal_legacy_shape() {
    let (ledger, _, action) = basis();
    let result = InvestigationResult::admit(
        "investigation-1",
        &ledger,
        &action,
        std::iter::empty(),
        None,
        "2026-10-19T00:00:00Z".into(),
    )
    .unwrap();
    let bytes = serde_json::to_vec(&result).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value.get("invocation_step_id").is_none());
    assert!(value.get("disclosure_hash").is_none());
    // Preserve struct field order in the legacy fingerprint, not map key order.
    let wire = String::from_utf8(bytes.clone()).unwrap();
    let legacy_wire = wire.replace(&result.result_id, "");
    let ordered_expected = format!(
        "investigation-result-v1:{}",
        crate::source_basis::domain_hash(
            b"cartograph:investigation-result:v1\0",
            legacy_wire.as_bytes()
        )
    );
    assert_eq!(result.result_id, ordered_expected);
    let restored: InvestigationResult = serde_json::from_slice(&bytes).unwrap();
    restored.validate(&ledger).unwrap();
    assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
    let mut invalid = restored;
    invalid.invocation_step_id = Some("step-2".into());
    assert!(invalid.validate(&ledger).is_err());
    invalid.schema_version = 99;
    assert!(invalid.validate(&ledger).is_err());
}

#[test]
fn disclosure_admission_retains_full_supplied_text_replay_guard() {
    let (ledger, disclosure, action) = basis();
    let prose = "The selected transition contains a rejection branch.";
    assert!(matches!(
        InvestigationResult::admit_with_disclosure(
            "investigation-1",
            &ledger,
            "step-2",
            &disclosure,
            &action,
            [prose],
            None,
            "2026-10-19T00:00:00Z".into()
        ),
        Err(InvestigationError::SourceReplay)
    ));
}

#[path = "coverage_result_tests.rs"]
mod coverage_result_tests;
