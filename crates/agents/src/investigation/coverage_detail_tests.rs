use super::tests::fixture;
use super::*;
use crate::investigation::*;
use llm::{EgressFirewall, EgressPolicy, OllamaProvider, bounded::CompletionLimits};
use std::time::Duration;

// AC-0239: acquisition, actual supply and finishing metadata are distinct;
// inventory contents do not manufacture source citations or inspection targets.
#[test]
fn coverage_detail_is_budgeted_supplied_and_disclosed_without_source_citations() {
    let (archive, request) = fixture();
    let previous = InvestigationInputLedger {
        schema_version: 2,
        coverage: Some(archive.proof.clone()),
        coverage_reads: vec![],
        artifact_reads: vec![],
        graph_snapshot_id: archive.proof.snapshot_id.clone(),
        scope_snapshot_id: archive.proof.snapshot_id.clone(),
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    let raw = serde_json::to_string(&archive.read(&request).unwrap()).unwrap();
    let mut current = previous.clone();
    current.revision = 1;
    current
        .coverage_reads
        .push(InvestigationCoverageReadManifest {
            request,
            response_hash: core_prov::content_hash(raw.as_bytes()),
            response_bytes: raw.len(),
        });
    archive.validate_reads(&current.coverage_reads).unwrap();
    let basis = InvestigationTurnBasis {
        input_ledger_hash: current.fingerprint().unwrap(),
        previous_ledger: Some(previous),
        completed_tool: Some(InvestigationTool::ReadCoverageInventory),
    };
    let mut wrong_tool = basis.clone();
    wrong_tool.completed_tool = Some(InvestigationTool::QueryContext);
    assert!(wrong_tool.validate(&current).is_err());
    let mut forged_initial = current.clone();
    forged_initial.revision = 0;
    assert!(
        InvestigationTurnBasis {
            input_ledger_hash: forged_initial.fingerprint().unwrap(),
            previous_ledger: None,
            completed_tool: None,
        }
        .validate(&forged_initial)
        .is_err()
    );
    let prepare = |raws: &[String]| {
        InvestigationInput::prepare_v6(
            "Explain the limits",
            SpecialistId::DomainAnalystV6,
            current.clone(),
            &[],
            &[],
            &[],
            &InvestigationUsage::default(),
            &basis,
            None,
            raws,
        )
    };
    assert!(prepare(&[]).is_err());
    assert!(prepare(&[format!("{raw} ")]).is_err());
    let input = prepare(std::slice::from_ref(&raw)).unwrap();
    // AC-0327: retained accounting includes detail privacy owners appended
    // after base input construction, not just the final prompt wire.
    input.assert_retained_workspace(&INVESTIGATION_V1_POLICY);
    let action = input.action("inv:detail".into()).unwrap();
    let prompt: serde_json::Value = serde_json::from_str(&action.payload.prompt).unwrap();
    assert_eq!(prompt["coverage_details"].as_array().unwrap().len(), 1);
    assert_eq!(prompt["turn_progress"]["phase"], "after_coverage_read");
    assert_eq!(
        prompt["turn_progress"]["raw_query_budget"]["consumed_bytes"],
        raw.len()
    );
    assert!(current.citations.is_empty());
    let provider =
        OllamaProvider::new("http://127.0.0.1:11434", "test", Duration::from_secs(1)).unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let prepared = firewall
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    let disclosure = input.seal_disclosure(&prepared).unwrap().unwrap();
    assert_eq!(disclosure.coverage_details.len(), 1);
    assert!(!disclosure.coverage_details[0].redacted);
    assert!(disclosure.sources.is_empty());
    let mut dropped = disclosure.clone();
    dropped.coverage_details.clear();
    assert!(dropped.validate(&current).is_err());
    let mut tampered = disclosure;
    tampered.coverage_details[0].disclosed_body_hash = "a".repeat(64);
    assert!(tampered.validate(&current).is_err());
    for _ in 0..3 {
        current
            .coverage_reads
            .push(current.coverage_reads[0].clone());
    }
    for read in &mut current.coverage_reads {
        read.response_bytes = MAX_QUERY_BYTES;
        read.request.max_bytes = MAX_QUERY_BYTES;
    }
    assert!(InvestigationQueryBudget::from_ledger(&current).is_err());
}
