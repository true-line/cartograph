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
    serde_json::to_string(&InvestigationArtifactPage {
        schema_version: 1,
        context_id: context_id.into(),
        artifact_id: "gap-register-index".into(),
        schema: "gap-register-index@1".into(),
        content_id: content_id.into(),
        offset,
        next_offset: (end < total).then_some(end),
        total_content_bytes: total,
        complete_system: false,
        content: content.into(),
    })
    .unwrap()
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
    let content_id = format!("spec-artifact-v1:{}", "c".repeat(64));
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
    other_content.request.expected_content_id =
        Some(format!("spec-artifact-v1:{}", "d".repeat(64)));
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
    // The retained page follows the coverage bodies; without it, or with a
    // non-canonical copy, no prompt is built.
    let prepare = |raws: &[String]| {
        InvestigationInput::prepare_v6(
            "Explain the gaps",
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
    assert_eq!(prepare(&[]).err(), Some(InvestigationError::InvalidInput));
    let pretty =
        serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(&raw).unwrap())
            .unwrap();
    assert!(prepare(&[pretty]).is_err());
    let input = prepare(std::slice::from_ref(&raw)).unwrap();
    let action = input.action("inv:artifact".into()).unwrap();
    let prompt: serde_json::Value = serde_json::from_str(&action.payload.prompt).unwrap();
    assert_eq!(prompt["artifact_pages"][0]["content"], "# Gaps\n");
    assert_eq!(prompt["turn_progress"]["phase"], "after_artifact_read");
    // Sealed as a supplied page, not a citation; dropping or forging it fails.
    let provider = llm::OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let prepared = llm::EgressFirewall::new(llm::EgressPolicy::default())
        .prepare_bounded(
            &provider,
            &action,
            &llm::bounded::CompletionLimits::default(),
        )
        .unwrap();
    let disclosure = input.seal_disclosure(&prepared).unwrap().unwrap();
    assert_eq!(disclosure.artifact_pages.len(), 1);
    assert!(!disclosure.artifact_pages[0].redacted);
    assert_eq!(
        disclosure.artifact_pages[0].disclosed_body_hash,
        manifest.response_hash
    );
    assert!(disclosure.sources.is_empty());
    disclosure.validate(&current).unwrap();
    let mut dropped = disclosure.clone();
    dropped.artifact_pages.clear();
    assert!(dropped.validate(&current).is_err());
    let mut forged = disclosure;
    forged.artifact_pages[0].disclosed_body_hash = "a".repeat(64);
    assert!(forged.validate(&current).is_err());
}
