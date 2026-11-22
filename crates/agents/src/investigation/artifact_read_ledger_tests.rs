use super::tests::fixture;
use crate::investigation::*;
use serde_json::json;

fn page_raw(
    context_id: &str,
    content_id: &str,
    offset: usize,
    content: &str,
    total: usize,
) -> String {
    let end = offset + content.len();
    json!({"schema_version":1,"context_id":context_id,"artifact_id":"gap-register-index",
        "schema":"gap-register-index@1","content_id":content_id,"offset":offset,
        "next_offset":(end < total).then_some(end),"total_content_bytes":total,"content":content})
    .to_string()
}

// AC-1812 (ADR-0089): an artifact page is a v2 ledger manifest charged to the
// shared acquisition budget; it is its own turn, never a citation, and older
// ledgers keep their bytes.
#[test]
fn artifact_reads_are_charged_ledger_manifests_with_their_own_turn() {
    let (archive, _) = fixture();
    let proof = archive.proof.clone();
    let previous = InvestigationInputLedger {
        schema_version: 2,
        coverage: Some(proof.clone()),
        coverage_reads: vec![],
        artifact_reads: vec![],
        graph_snapshot_id: proof.snapshot_id.clone(),
        scope_snapshot_id: proof.snapshot_id.clone(),
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    // An empty list is not serialized, so existing fingerprints are unchanged.
    assert!(
        serde_json::to_value(&previous)
            .unwrap()
            .get("artifact_reads")
            .is_none()
    );
    let content_id = "c".repeat(64);
    let raw = page_raw(&proof.context_id, &content_id, 0, "# Gaps\n", 20);
    let request = InvestigationArtifactRead {
        context_id: proof.context_id.clone(),
        artifact_id: "gap-register-index".into(),
        expected_content_id: None,
        offset: 0,
        max_bytes: 4096,
    };
    let manifest = InvestigationArtifactReadManifest {
        request: request.clone(),
        content_id: content_id.clone(),
        response_hash: core_prov::content_hash(raw.as_bytes()),
        response_bytes: raw.len(),
        next_offset: Some(7),
    };
    let page = manifest.admit_response(&proof, &raw).unwrap();
    assert_eq!(page.content, "# Gaps\n");
    assert!(manifest.admit_response(&proof, &format!("{raw} ")).is_err());
    let mut wrong_end = manifest.clone();
    wrong_end.next_offset = Some(8);
    assert!(wrong_end.admit_response(&proof, &raw).is_err());
    let mut other_content = manifest.clone();
    other_content.request.expected_content_id = Some("d".repeat(64));
    other_content.request.offset = 7;
    assert!(other_content.validate(&proof).is_err());

    let mut current = previous.clone();
    current.revision = 1;
    current.artifact_reads.push(manifest.clone());
    let basis = InvestigationTurnBasis {
        input_ledger_hash: current.fingerprint().unwrap(),
        previous_ledger: Some(previous.clone()),
        completed_tool: Some(InvestigationTool::ReadArtifact),
    };
    let progress = basis.progress(&current, &json!({})).unwrap();
    assert_eq!(progress.phase, InvestigationTurnPhase::AfterArtifactRead);
    assert_eq!(progress.raw_query_budget.consumed_bytes, raw.len());
    assert_eq!(
        serde_json::to_value(progress.last_completed_action).unwrap(),
        json!({"type":"read_artifact","read_index":0,"artifact_id":"gap-register-index",
            "content_id":content_id,"offset":0,"next_offset":7,"response_bytes":raw.len()})
    );
    for tool in [
        InvestigationTool::QueryContext,
        InvestigationTool::ReadEvidence,
        InvestigationTool::ReadCoverageInventory,
    ] {
        let mut wrong = basis.clone();
        wrong.completed_tool = Some(tool);
        assert!(wrong.validate(&current).is_err(), "{tool:?}");
    }
    // An identical read repeats acquired metadata; a continuation does not.
    let action = QualifiedInvestigationAction::ReadArtifact(request.clone());
    assert!(action.repeats_acquired_metadata(&current));
    let mut next = request;
    next.offset = 7;
    next.expected_content_id = Some(content_id);
    assert!(!QualifiedInvestigationAction::ReadArtifact(next).repeats_acquired_metadata(&current));
    // Artifact reads need the qualified v2 ledger.
    let mut legacy = current.clone();
    legacy.schema_version = 1;
    legacy.coverage = None;
    assert!(legacy.validate().is_err());
    // A page cannot become a prompt before its supply lands.
    assert_eq!(
        InvestigationInput::prepare_v6(
            "Explain the gaps",
            SpecialistId::DomainAnalystV6,
            current,
            &[],
            &[],
            &[],
            &InvestigationUsage::default(),
            &basis,
            None,
            &[],
        )
        .err(),
        Some(InvestigationError::InvalidInput)
    );
}
