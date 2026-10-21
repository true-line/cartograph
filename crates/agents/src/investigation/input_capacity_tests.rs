//! Scripted capacity witnesses only. No provider request is dispatched.
use super::*;
use crate::investigation::{obligations::*, staged_refusal::ProtectedCompletionMetadata};
use crate::{AgentEvidence, TaskFactKey};

struct CapacityFixture {
    request: StagedObligationRequest,
    ledger: InvestigationInputLedger,
    pages: Vec<String>,
    evidence: Vec<AgentEvidence>,
    metadata: ProtectedCompletionMetadata,
}
impl CapacityFixture {
    fn new(source_obligations: usize, evidence_bytes: usize) -> Self {
        Self::with_items(source_obligations, evidence_bytes, source_obligations)
    }
    fn with_items(source_obligations: usize, evidence_bytes: usize, evidence_items: usize) -> Self {
        let policy = &INVESTIGATION_V2_POLICY;
        let (mut target, mut ledger, _) = crate::investigation::tests::inspection_tests::fixture();
        let qualified = initial();
        ledger.schema_version = 2;
        ledger.graph_snapshot_id = qualified.graph_snapshot_id;
        ledger.scope_snapshot_id = qualified.scope_snapshot_id;
        ledger.coverage = qualified.coverage;
        target.graph_snapshot_id = ledger.graph_snapshot_id.clone();
        target.fact = TaskFactKey::Node { id: "a".into() };
        target.source.repo = "a".into();
        target.source.path = "a".into();
        target.source.commit_sha = "a".into();
        target.captured.file.path = "a".into();
        target.captured.file.byte_len = (evidence_bytes + 56) as u64;
        target.captured.file.digest = content_hash(&vec![b'x'; evidence_bytes + 56]);
        ledger.selected_facts[0].fact = target.fact.clone();
        ledger.selected_facts[0].binding.as_mut().unwrap().repo_key = "a".into();
        ledger.receipt_references[0].repo_key = "a".into();
        let source_citation = ledger.citations[1].clone();
        ledger.citations.truncate(1);
        ledger.citations[0].fact = target.fact.clone();
        let mut evidence = Vec::new();
        let mut obligations = Vec::new();
        let mut offset = 0;
        for index in 0..source_obligations.max(evidence_items) {
            let length = if evidence_bytes == 0 {
                1
            } else if index < evidence_items {
                evidence_bytes / evidence_items
                    + usize::from(index < evidence_bytes % evidence_items)
            } else {
                1
            };
            let mut selected = target.clone();
            selected.index = index as u32;
            selected.receipt_inventory_index = index as u32;
            selected.source.byte_start = offset as u64;
            selected.source.byte_end = (offset + length) as u64;
            selected.captured.byte_start = selected.source.byte_start;
            selected.captured.byte_end = selected.source.byte_end;
            selected.target_id = selected.content_id().unwrap();
            selected.validate().unwrap();
            if index < source_obligations {
                let mut item = Obligation::Source {
                    obligation_id: String::new(),
                    target: Box::new(selected.clone()),
                };
                let id = item.content_id().unwrap();
                if let Obligation::Source { obligation_id, .. } = &mut item {
                    *obligation_id = id;
                }
                obligations.push(item);
            }
            if evidence_bytes != 0 && index < evidence_items {
                let text = "x".repeat(length);
                let id = format!("evidence-{index}");
                let mut citation = source_citation.clone();
                citation.citation_id = id.clone();
                citation.fact = target.fact.clone();
                citation.index = Some(index as u32);
                citation.source = Some(selected.source.clone());
                citation.text_hash = Some(content_hash(text.as_bytes()));
                if let InvestigationEvidenceOrigin::CapturedPrimarySource {
                    receipt_inventory_index,
                    captured,
                    ..
                } = &mut citation.origin
                {
                    *receipt_inventory_index = index as u32;
                    *captured = selected.captured;
                }
                ledger.citations.push(citation);
                evidence.push(AgentEvidence {
                    id,
                    source: selected.source,
                    text,
                });
            }
            offset += length;
        }
        let proof = ledger.coverage.as_ref().unwrap();
        let raw = json!({"context":{"context_id":proof.context_id,"coverage":proof.qualification,
            "response":{"snapshot_id":ledger.scope_snapshot_id,"view":"recovered_context","total_selected":1,
            "facts":[{"reference":target.fact,"label":"BusinessRule"}],"next_cursor":null}},
            "evidence_options":[{"fact":target.fact,"origin":"captured_primary_source","ranges":[]}]}).to_string();
        ledger.queries[0].response_hash = content_hash(raw.as_bytes());
        ledger.queries[0].response_bytes = raw.len();
        ledger.validate_with_policy(policy).unwrap();
        let request = StagedObligationRequest {
            schema_version: 5,
            request_nonce: "capacity-witness".into(),
            specialist_id: "domain-analyst@8".into(),
            obligations: ObligationEnvelope {
                schema_version: 2,
                contract: "investigation-completion-obligations@2".into(),
                obligations,
            },
            inspection_requirements: None,
            question: "Explain recovered behavior".into(),
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            limit_profile: policy.profile().into(),
            expected_graph_revision: Some(ledger.graph_snapshot_id.clone()),
            expected_context_id: proof.context_id.clone(),
            conversation_id: None,
            parent_id: None,
        }
        .normalized_with_policy(policy)
        .unwrap();
        let metadata = ProtectedCompletionMetadata {
            schema_version: 1,
            request_hash: request.intent_hash_with_policy(policy).unwrap(),
            obligations_hash: request.obligations.fingerprint_with_policy(policy).unwrap(),
            completed_attempt_ids: (0..policy.max_completed_attempts())
                .map(|index| content_hash(format!("attempt-{index}").as_bytes()))
                .collect(),
            refusal_count: 0,
            last_refusal_id: None,
            feedback: None,
        };
        metadata.validate_with_policy(policy).unwrap();
        Self {
            request,
            ledger,
            pages: vec![raw],
            evidence,
            metadata,
        }
    }
    fn prepare(&self) -> Result<InvestigationInput, InvestigationError> {
        let policy = &INVESTIGATION_V2_POLICY;
        let mut previous = self.ledger.clone();
        previous.revision -= 1;
        let tool = if self.evidence.is_empty() {
            previous.queries.clear();
            previous.selected_facts.clear();
            previous.citations.clear();
            previous.receipt_references.clear();
            InvestigationTool::QueryContext
        } else {
            previous.citations.pop();
            InvestigationTool::ReadEvidence
        };
        let basis = InvestigationTurnBasis {
            input_ledger_hash: self.ledger.fingerprint_with_policy(policy).unwrap(),
            previous_ledger: Some(previous),
            completed_tool: Some(tool),
        };
        InvestigationInput::prepare_v7_with_policy(
            &self.request.question,
            SpecialistId::DomainAnalystV8,
            self.ledger.clone(),
            &self.pages,
            &self.evidence,
            &[],
            &InvestigationUsage::default(),
            &basis,
            &self.request,
            &self.metadata,
            &[],
            policy,
        )
    }
}

#[test]
fn v2_capacity_intersection_is_enforced_before_firewall_sealing() {
    // AC-0281: independent aggregate ceilings never enlarge the 128 KiB call.
    use llm::bounded::CompletionLimits;
    use llm::{EgressFirewall, EgressPolicy, OllamaProvider};
    use std::time::Duration;
    let maximum = CapacityFixture::new(56, 64 * 1024);
    assert_eq!(maximum.evidence.len(), 56);
    assert_eq!(
        maximum
            .evidence
            .iter()
            .map(|item| item.text.len())
            .sum::<usize>(),
        64 * 1024
    );
    assert_eq!(maximum.metadata.completed_attempt_ids.len(), 112);
    assert!(matches!(
        maximum.prepare(),
        Err(InvestigationError::LimitExceeded)
    ));
    // Synthetic source-only proxy for the 37-obligation measured shape. This
    // preparation/sealing witness makes no claim about a provider or execution.
    let mut representative = CapacityFixture::new(37, 33889);
    representative.metadata.completed_attempt_ids.truncate(37);
    let representative_input = representative.prepare().unwrap();
    eprintln!(
        "V2 synthetic 37-source/33889-byte/37-attempt: request={}, prompt={}, payload={}",
        serde_json::to_vec(&representative.request).unwrap().len(),
        representative_input.payload.prompt.len(),
        serde_json::to_vec(&representative_input.payload)
            .unwrap()
            .len()
    );
    let maximum_metadata = CapacityFixture::new(37, 33889);
    assert!(matches!(
        maximum_metadata.prepare(),
        Err(InvestigationError::LimitExceeded)
    ));
    let mut over_capacity = representative_input.payload.clone();
    let mut over_prompt: Value = serde_json::from_str(&over_capacity.prompt).unwrap();
    over_prompt["completion_metadata"] = serde_json::to_value(&maximum_metadata.metadata).unwrap();
    over_capacity.prompt = serde_json::to_string(&over_prompt).unwrap();
    eprintln!(
        "V2 same source/bytes with112attempt IDs: prompt={}, payload={}",
        over_capacity.prompt.len(),
        serde_json::to_vec(&over_capacity).unwrap().len()
    );
    assert!(serde_json::to_vec(&over_capacity).unwrap().len() > 128 * 1024);
    let fixture = CapacityFixture::new(56, 0);
    let input = fixture.prepare().unwrap();
    eprintln!(
        "V2 56source/noevidence/112attempt: request={}, prompt={}, payload={}",
        serde_json::to_vec(&fixture.request).unwrap().len(),
        input.payload.prompt.len(),
        serde_json::to_vec(&input.payload).unwrap().len()
    );
    // Before V2 target deduplication this same 56-source/no-evidence fixture
    // measured request=57621, prompt=128723, payload=147041 bytes, including
    // 50903 bytes of duplicate inspection descriptors and a 57353-byte contract.
    // Uncapped diagnostic serialization only: replace the exact request-owned
    // blocks in an actual prepared input. This never reaches the firewall.
    let targets = fixture
        .request
        .obligations
        .sources_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let mut payload = input.payload.clone();
    let mut prompt: Value = serde_json::from_str(&payload.prompt).unwrap();
    prompt["completion_contract"] = serde_json::to_value(InvestigationCompletionContract {
        schema_version: 1,
        request_hash: fixture.metadata.request_hash.clone(),
        obligations_hash: fixture.metadata.obligations_hash.clone(),
        obligations: fixture.request.obligations.clone(),
    })
    .unwrap();
    prompt["completion_metadata"] = serde_json::to_value(&fixture.metadata).unwrap();
    prompt["inspection_requirements"] = serde_json::to_value(&targets).unwrap();
    prompt["inspection_requirements_hash"] =
        inspection_requirements_hash_with_policy(&targets, &INVESTIGATION_V2_POLICY)
            .unwrap()
            .into();
    prompt["inspection_progress"] = json!({"requirements":targets.iter().map(|target|json!({"target_id":target.target_id,"acquired":false,"exact_supply":"determined_by_finishing_disclosure"})).collect::<Vec<_>>()});
    payload.prompt = serde_json::to_string(&prompt).unwrap();
    eprintln!(
        "V2 56source diagnostic: request={}, prompt={}, payload={}, inspection_requirements={}, completion_contract={}",
        serde_json::to_vec(&fixture.request).unwrap().len(),
        payload.prompt.len(),
        serde_json::to_vec(&payload).unwrap().len(),
        serde_json::to_vec(&prompt["inspection_requirements"])
            .unwrap()
            .len(),
        serde_json::to_vec(&prompt["completion_contract"])
            .unwrap()
            .len()
    );
    assert!(serde_json::to_vec(&payload).unwrap().len() > 128 * 1024);
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let mut transport = CompletionLimits::default().values();
    transport.max_spans = u32::try_from(INVESTIGATION_V2_POLICY.limits().evidence_items).unwrap();
    let limits = CompletionLimits::new(transport).unwrap();
    for (index, input) in [input, representative_input].into_iter().enumerate() {
        let action = input.action(format!("inv:capacity-{index}")).unwrap();
        let prepared = firewall
            .prepare_bounded(&provider, &action, &limits)
            .unwrap();
        input
            .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
            .unwrap()
            .unwrap();
    }
}
