use super::*;
use context_hub::{
    ContextSnapshot,
    coverage::{CoverageQualification, INVENTORY_VERSION, ScopedRepo},
};
use core_graph::coverage::{CoverageRecord, StoredCoverage};
use llm::bounded::CompletionLimits;
use llm::{EgressFirewall, EgressPolicy, OllamaProvider};
use serde_json::json;
use std::time::Duration;

fn ledger(qualified: bool) -> InvestigationInputLedger {
    let snapshot = ContextSnapshot::new(vec![], vec![]).unwrap();
    let record = CoverageRecord {
        repo_key: "acme/api".into(),
        registered_source_id: Some("src:api".into()),
        ingest_id: "job:fixture".into(),
        config_hash: "plugins:fixture".into(),
        inventory_json: json!({"inventory":INVENTORY_VERSION,"repo":"acme/api","commit":"a".repeat(40),"layer_hints":[],
            "producers":[],"unsupported":[],"limited":false})
        .to_string(),
    };
    let stored = StoredCoverage {
        inventory_hash: record.inventory_hash(),
        record,
    };
    let proof = CoverageQualification::new(
        &[ScopedRepo {
            repo_key: "acme/api".into(),
            registered_source_id: Some("src:api".into()),
            config_hash: "plugins:fixture".into(),
            layer_hints: None,
        }],
        &[stored],
    )
    .unwrap()
    .durable_proof(snapshot.id())
    .unwrap();
    InvestigationInputLedger {
        coverage_reads: vec![],
        artifact_reads: vec![],
        schema_version: if qualified { 2 } else { 1 },
        coverage: qualified.then_some(proof),
        graph_snapshot_id: snapshot.id().into(),
        scope_snapshot_id: snapshot.id().into(),
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    }
}
fn target(ledger: &InvestigationInputLedger) -> InvestigationInspectionTarget {
    let mut target:InvestigationInspectionTarget=serde_json::from_value(json!({
        "schema_version":1,"target_id":"","graph_snapshot_id":ledger.graph_snapshot_id,
        "fact":{"kind":"node","id":"node:checkout"},"fact_digest":format!("node-v1:{}","a".repeat(64)),
        "registered_source_id":format!("src_{}","d".repeat(32)),"receipt_id":format!("ts-primary-v2:{}","c".repeat(64)),
        "receipt_inventory_index":3,"role":"provenance","index":0,
        "source":{"repo":"acme/api","path":"guard.ts","byte_start":0,"byte_end":12,"commit_sha":"a".repeat(40)},
        "captured":{"file":{"source_id":format!("src_{}","d".repeat(32)),"capture_id":format!("capture-v1:{}","e".repeat(64)),
        "path":"guard.ts","digest":content_hash(b"private text"),"byte_len":12},"byte_start":0,"byte_end":12}
    })).unwrap();
    target.target_id = target.content_id().unwrap();
    target.validate().unwrap();
    target
}
fn finish() -> InvestigationAction {
    InvestigationAction::Finish {
        findings: vec![],
        knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
        limitations: vec![
            "No relevant source was acquired; project coverage remains unknown.".into(),
        ],
    }
}
fn prepared(
    ledger: &InvestigationInputLedger,
    version: u32,
    targets: Option<&[InvestigationInspectionTarget]>,
) -> (InvestigationInput, InvestigationDisclosure) {
    let basis = InvestigationTurnBasis {
        input_ledger_hash: ledger.fingerprint().unwrap(),
        previous_ledger: None,
        completed_tool: None,
    };
    let usage = InvestigationUsage::default();
    let input = match version {
        3 => InvestigationInput::prepare(
            "Inspect behavior",
            SpecialistId::DomainAnalystV3,
            ledger.clone(),
            &[],
            &[],
            &[],
            &usage,
        ),
        5 => InvestigationInput::prepare_v5(
            "Inspect behavior",
            SpecialistId::DomainAnalystV5,
            ledger.clone(),
            &[],
            &[],
            &[],
            &usage,
            &basis,
            targets.unwrap(),
        ),
        _ => InvestigationInput::prepare_v6(
            "Inspect behavior",
            SpecialistId::DomainAnalystV6,
            ledger.clone(),
            &[],
            &[],
            &[],
            &usage,
            &basis,
            targets,
            &[],
        ),
    }
    .unwrap();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let action = input.action("inv:fixture:step:1".into()).unwrap();
    let prepared = firewall
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    let disclosure = input.seal_disclosure(&prepared).unwrap().unwrap();
    (input, disclosure)
}
fn result(input: &InvestigationInput, disclosure: &InvestigationDisclosure) -> InvestigationResult {
    input
        .admit_result_for_step(
            "inv:fixture",
            "step:1",
            Some(disclosure),
            &finish(),
            None,
            "2026-10-19T00:00:00Z".into(),
        )
        .unwrap()
}

// AC-0239: historical versions retain exact serialized result identity.
#[test]
fn qualified_result_preserves_frozen_v1_v2_v3_identities() {
    let ledger = ledger(false);
    let v1 = InvestigationResult::admit(
        "inv:fixture",
        &ledger,
        &finish(),
        std::iter::empty(),
        None,
        "2026-10-19T00:00:00Z".into(),
    )
    .unwrap();
    let (input, disclosure) = prepared(&ledger, 3, None);
    let v2 = result(&input, &disclosure);
    let (input, disclosure) = prepared(&ledger, 5, Some(&[target(&ledger)]));
    let v3 = result(&input, &disclosure);
    for (result, expected) in [
        (
            v1,
            "investigation-result-v1:a4f29317827299828e05d9dcd68bef7e0fec8af8767b7d3988421e5daf9f392e",
        ),
        (
            v2,
            "investigation-result-v2:e22bedcffe6d12386eafd7c54e7a02ae62eab0b4f12692a884a3155af92f4ab1",
        ),
        (
            v3,
            "investigation-result-v3:7850096ae30745b446ce8b16c194abe847162888fd9fdcdfc00c69e1186eb8c5",
        ),
    ] {
        assert_eq!(result.result_id, expected);
        assert!(
            serde_json::to_value(&result)
                .unwrap()
                .get("coverage")
                .is_none()
        );
    }
}

// Independent protocol rehash keeps semantic tamper tests from merely proving
// that changing bytes changes a hash. Field order follows the public result wire.
fn rehash(result: &mut InvestigationResult) {
    result.result_id.clear();
    result.result_id = format!(
        "investigation-result-v{}:{}",
        result.schema_version,
        crate::source_basis::domain_hash(
            format!(
                "cartograph:investigation-result:v{}\0",
                result.schema_version
            )
            .as_bytes(),
            &serde_json::to_vec(result).unwrap()
        )
    );
}

// AC-0239: result coverage and inspection are distinct immutable attestations.
#[test]
fn qualified_result_roundtrip_binds_finishing_coverage_with_optional_inspection() {
    let ledger = ledger(true);
    for targeted in [false, true] {
        let targets = [target(&ledger)];
        let (input, disclosure) = prepared(&ledger, 6, targeted.then_some(targets.as_slice()));
        let result = result(&input, &disclosure);
        assert_eq!(result.schema_version, 4);
        assert!(result.result_id.starts_with("investigation-result-v4:"));
        assert_eq!(result.coverage, disclosure.coverage);
        assert_eq!(result.inspection_assessment.is_some(), targeted);
        if let Some(assessment) = &result.inspection_assessment {
            assert_eq!(assessment.requirements.len(), 1);
            assert_eq!(
                assessment.requirements[0].status,
                InvestigationInspectionStatus::Unmet
            );
            assert!(
                !assessment.requirements[0].acquired
                    && !assessment.requirements[0].supplied
                    && !assessment.requirements[0].cited
            );
        }
        let bytes = serde_json::to_vec(&result).unwrap();
        let restored: InvestigationResult =
            decode_record(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(result, restored);
        restored
            .validate_with_disclosure(&ledger, "step:1", &disclosure)
            .unwrap();
        assert!(restored.validate(&ledger).is_err());
        assert!(
            restored
                .validate_with_disclosure(&ledger, "step:2", &disclosure)
                .is_err()
        );
        let mut changed = disclosure.clone();
        changed.payload_hash = content_hash(b"another finishing invocation");
        assert!(
            restored
                .validate_with_disclosure(&ledger, "step:1", &changed)
                .is_err()
        );
        let mut nested = serde_json::to_value(&result).unwrap();
        nested["coverage"]["current"] = true.into();
        assert!(serde_json::from_value::<InvestigationResult>(nested).is_err());
    }
}

// AC-0239: even independently rehashed records must match the retained proof
// and exact finishing disclosure, not only the graph snapshot.
#[test]
fn qualified_result_rejects_rehashed_public_attestation_mutation_and_proof_swap() {
    let ledger = ledger(true);
    let (input, disclosure) = prepared(&ledger, 6, None);
    let original = result(&input, &disclosure);
    for field in [
        "missing",
        "context_id",
        "coverage_id",
        "representation_hash",
    ] {
        let mut changed = original.clone();
        match field {
            "missing" => changed.coverage = None,
            "context_id" => {
                changed.coverage.as_mut().unwrap().context_id =
                    format!("context-v2:{}", "f".repeat(64))
            }
            "coverage_id" => {
                changed.coverage.as_mut().unwrap().coverage_id =
                    format!("coverage-context-v1:{}", "f".repeat(64))
            }
            _ => {
                changed.coverage.as_mut().unwrap().representation_hash =
                    content_hash(b"substituted public representation")
            }
        }
        rehash(&mut changed);
        assert!(
            changed
                .validate_with_disclosure(&ledger, "step:1", &disclosure)
                .is_err()
        );
    }
    let mut another = ledger.clone();
    another.coverage = Some(
        CoverageQualification::new(
            &[ScopedRepo {
                repo_key: "acme/other".into(),
                registered_source_id: None,
                config_hash: "unknown".into(),
                layer_hints: None,
            }],
            &[],
        )
        .unwrap()
        .durable_proof(&another.scope_snapshot_id)
        .unwrap(),
    );
    let (_, other_disclosure) = prepared(&another, 6, None);
    assert_eq!(ledger.graph_snapshot_id, another.graph_snapshot_id);
    let mut forged = original.clone();
    forged.input_ledger_hash = another.fingerprint().unwrap();
    forged.disclosure_hash = Some(other_disclosure.fingerprint(&another).unwrap());
    rehash(&mut forged);
    // Other outer identities were updated, but the old mandatory coverage was not.
    assert!(
        forged
            .validate_with_disclosure(&another, "step:1", &other_disclosure)
            .is_err()
    );
    let mut tampered = ledger.clone();
    tampered.coverage.as_mut().unwrap().basis[0].ingest_id = "edited-preparation-basis".into();
    assert!(
        original
            .validate_with_disclosure(&tampered, "step:1", &disclosure)
            .is_err()
    );
}

// AC-0239: absence of inspection requirements never fabricates an assessment.
#[test]
fn qualified_result_rejects_inserted_or_dropped_independent_inspection_assessment() {
    let ledger = ledger(true);
    let (plain_input, plain_disclosure) = prepared(&ledger, 6, None);
    let plain = result(&plain_input, &plain_disclosure);
    let (targeted_input, targeted_disclosure) = prepared(&ledger, 6, Some(&[target(&ledger)]));
    let targeted = result(&targeted_input, &targeted_disclosure);
    let mut inserted = plain;
    inserted.inspection_assessment = targeted.inspection_assessment.clone();
    rehash(&mut inserted);
    assert!(
        inserted
            .validate_with_disclosure(&ledger, "step:1", &plain_disclosure)
            .is_err()
    );
    let mut dropped = targeted;
    dropped.inspection_assessment = None;
    rehash(&mut dropped);
    assert!(
        dropped
            .validate_with_disclosure(&ledger, "step:1", &targeted_disclosure)
            .is_err()
    );
}

#[test]
fn qualified_result_requires_disclosure_and_forbids_legacy_schema_with_qualified_ledger() {
    let qualified = ledger(true);
    assert!(
        InvestigationResult::admit(
            "inv:fixture",
            &qualified,
            &finish(),
            std::iter::empty(),
            None,
            "now".into()
        )
        .is_err()
    );
    let (input, disclosure) = prepared(&qualified, 6, None);
    let result = result(&input, &disclosure);
    for version in [1, 2, 3] {
        let mut downgrade = result.clone();
        downgrade.schema_version = version;
        downgrade.coverage = None;
        rehash(&mut downgrade);
        assert!(downgrade.validate(&qualified).is_err());
        assert!(
            downgrade
                .validate_with_disclosure(&qualified, "step:1", &disclosure)
                .is_err()
        );
    }
    let legacy = ledger(false);
    let (input, disclosure) = prepared(&legacy, 3, None);
    let mut invalid = result.clone();
    invalid.input_ledger_hash = legacy.fingerprint().unwrap();
    invalid.disclosure_hash = Some(disclosure.fingerprint(&legacy).unwrap());
    rehash(&mut invalid);
    assert!(
        invalid
            .validate_with_disclosure(&legacy, "step:1", &disclosure)
            .is_err()
    );
    assert!(
        input
            .admit_result_for_step("inv:fixture", "step:1", None, &finish(), None, "now".into())
            .is_err()
    );
}

// AC-0239: exact supplied source can discharge an optional target independently
// of the mandatory aggregate qualification, without becoming semantic success.
#[test]
fn qualified_result_keeps_actual_supplied_inspection_separate_from_coverage() {
    use crate::{AgentEvidence, InputClosureStatus, PrimarySourceScope, TaskSourceAssociation};
    let mut ledger = ledger(true);
    let target = target(&ledger);
    ledger.revision = 2;
    ledger.selected_facts = vec![TaskFactSelection {
        fact: target.fact.clone(),
        fact_digest: target.fact_digest.clone(),
        binding: Some(TaskSourceAssociation {
            repo_key: target.source.repo.clone(),
            receipt_id: target.receipt_id.clone(),
            emitted_fact_digest: target.fact_digest.clone(),
        }),
    }];
    ledger.receipt_references = vec![InvestigationReceiptReference {
        source_id: target.registered_source_id.clone(),
        repo_key: target.source.repo.clone(),
        receipt_id: target.receipt_id.clone(),
    }];
    ledger.citations = vec![
        InvestigationCitation {
            citation_id: "fact:checkout".into(),
            fact: target.fact.clone(),
            fact_digest: target.fact_digest.clone(),
            source: None,
            role: None,
            index: None,
            text_hash: None,
            origin: InvestigationEvidenceOrigin::GraphMetadata,
        },
        InvestigationCitation {
            citation_id: "source:checkout".into(),
            fact: target.fact.clone(),
            fact_digest: target.fact_digest.clone(),
            source: Some(target.source.clone()),
            role: Some(target.role),
            index: Some(target.index),
            text_hash: Some(content_hash(b"private text")),
            origin: InvestigationEvidenceOrigin::CapturedPrimarySource {
                registered_source_id: target.registered_source_id.clone(),
                receipt_id: target.receipt_id.clone(),
                receipt_inventory_index: target.receipt_inventory_index,
                captured: target.captured.clone(),
                scope: PrimarySourceScope::PrimarySourceOnly,
                input_closure: InputClosureStatus::InputClosureNotEstablished,
            },
        },
    ];
    let proof = ledger.coverage.as_ref().unwrap();
    let raw=json!({"context":{"context_id":proof.context_id,"coverage":proof.qualification,"response":{
        "snapshot_id":ledger.scope_snapshot_id,"view":"recovered_context","total_selected":1,"next_cursor":null,
        "facts":[{"reference":target.fact,"label":"Function","properties":{},"provenance":null,"provenance_problem":"missing","confidence_tier":"Gap"}]}},
        "evidence_options":[{"fact":target.fact,"origin":"captured_primary_source","ranges":[]}]}).to_string();
    ledger.queries = vec![InvestigationQueryManifest {
        query: InvestigationQuery {
            scope: InvestigationScope::All,
            kind: None,
            labels: vec![],
            max_facts: 12,
            max_bytes: 16384,
            cursor: None,
        },
        response_hash: content_hash(raw.as_bytes()),
        response_bytes: raw.len(),
        returned_facts: 1,
        total_selected: 1,
        has_more: false,
    }];
    let mut previous = ledger.clone();
    previous.revision = 1;
    previous.citations.pop();
    let basis = InvestigationTurnBasis {
        input_ledger_hash: ledger.fingerprint().unwrap(),
        previous_ledger: Some(previous),
        completed_tool: Some(InvestigationTool::ReadEvidence),
    };
    let input = InvestigationInput::prepare_v6(
        "Inspect behavior",
        SpecialistId::DomainAnalystV6,
        ledger.clone(),
        &[raw],
        &[AgentEvidence {
            id: "source:checkout".into(),
            source: target.source.clone(),
            text: "private text".into(),
        }],
        &[],
        &InvestigationUsage::default(),
        &basis,
        Some(&[target]),
        &[],
    )
    .unwrap();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let action = input.action("inv:fixture:step:1".into()).unwrap();
    let prepared = firewall
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    let disclosure = input.seal_disclosure(&prepared).unwrap().unwrap();
    let result = result(&input, &disclosure);
    assert_eq!(result.schema_version, 4);
    assert_eq!(result.coverage, disclosure.coverage);
    let observed = &result.inspection_assessment.as_ref().unwrap().requirements[0];
    assert!(observed.acquired && observed.supplied);
    assert!(!observed.cited);
    assert_eq!(observed.status, InvestigationInspectionStatus::Supplied);
    assert_eq!(
        result.knowledge_completeness,
        KnowledgeCompleteness::InsufficientEvidence
    );
    result
        .validate_with_disclosure(&ledger, "step:1", &disclosure)
        .unwrap();
}
