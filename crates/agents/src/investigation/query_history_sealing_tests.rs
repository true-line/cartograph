use super::*;
use context_hub::coverage::CoverageQualification;
use llm::{EgressFirewall, EgressPolicy, OllamaProvider, bounded::CompletionLimits};
use std::time::Duration;

fn input(different_facts: bool) -> (InvestigationInput, InvestigationInspectionTarget) {
    let policy = &INVESTIGATION_V2_POLICY;
    let (target, mut ledger, _) = super::super::tests::inspection_tests::fixture();
    ledger.schema_version = 2;
    ledger.coverage = Some(
        CoverageQualification::new(&[], &[])
            .unwrap()
            .durable_proof(&ledger.graph_snapshot_id)
            .unwrap(),
    );
    ledger
        .citations
        .retain(|citation| matches!(citation.origin, InvestigationEvidenceOrigin::GraphMetadata));
    ledger.queries.clear();
    ledger.revision = 0;
    let mut source = obligations::Obligation::Source {
        obligation_id: String::new(),
        target: Box::new(target.clone()),
    };
    let id = source.content_id().unwrap();
    if let obligations::Obligation::Source { obligation_id, .. } = &mut source {
        *obligation_id = id;
    }
    let request = obligations::StagedObligationRequest {
        schema_version: 6,
        request_nonce: "interned-query-indexes".into(),
        specialist_id: "domain-analyst@9".into(),
        obligations: obligations::ObligationEnvelope {
            schema_version: 2,
            contract: "investigation-completion-obligations@2".into(),
            obligations: vec![source],
        },
        inspection_requirements: None,
        question: "Inspect the retained evidence.".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: policy.profile().into(),
        expected_graph_revision: Some(ledger.graph_snapshot_id.clone()),
        expected_context_id: ledger.coverage.as_ref().unwrap().context_id.clone(),
        conversation_id: None,
        parent_id: None,
    };
    let metadata = staged_refusal::ProtectedCompletionMetadata {
        schema_version: 1,
        request_hash: request.intent_hash_with_policy(policy).unwrap(),
        obligations_hash: request.obligations.fingerprint_with_policy(policy).unwrap(),
        completed_attempt_ids: vec![],
        refusal_count: 0,
        last_refusal_id: None,
        feedback: None,
    };
    let mut pages = Vec::new();
    let mut previous = None;
    for index in 0..2 {
        previous = Some(ledger.clone());
        let note = if different_facts && index == 1 {
            "different"
        } else {
            "same"
        };
        let raw=json!({"context":{"context_id":request.expected_context_id,
            "coverage":ledger.coverage.as_ref().unwrap().qualification,
            "response":{"snapshot_id":ledger.scope_snapshot_id,"view":"recovered_graph","total_selected":1,
                "facts":[{"reference":target.fact,"properties":{"note":note}}],"next_cursor":null}},
            "evidence_options":[{"fact":target.fact,"origin":"captured_primary_source_metadata","ranges":[]}]}).to_string();
        ledger.revision += 1;
        ledger.queries.push(InvestigationQueryManifest {
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
        });
        pages.push(raw);
    }
    let basis = InvestigationTurnBasis {
        input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
        previous_ledger: previous,
        completed_tool: Some(InvestigationTool::QueryContext),
    };
    let usage = InvestigationUsage {
        model_invocations: 2,
        tool_actions: 2,
        selected_facts: 1,
        generated_token_reservations: 4096,
        ..InvestigationUsage::default()
    };
    let prepared = InvestigationInput::prepare_v8_with_policy(
        &request.question,
        SpecialistId::DomainAnalystV9,
        ledger,
        &pages,
        &[],
        &[],
        &usage,
        &basis,
        &request,
        &metadata,
        &[],
        policy,
    )
    .unwrap();
    (prepared, target)
}

#[test]
fn query_history_sealing_rejects_indexes_outside_actual_interned_arrays() {
    // AC-0286: duplicate acquisitions do not create duplicate interned slots.
    // Matching original+prepared action identities must not bypass this check.
    let policy = &INVESTIGATION_V2_POLICY;
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    for different_facts in [false, true] {
        let (input, target) = input(different_facts);
        let original = input.action("inv:interned-indexes".into()).unwrap();
        let valid = firewall
            .prepare_bounded(&provider, &original, &CompletionLimits::default())
            .unwrap();
        input.seal_disclosure_with_policy(&valid, policy).unwrap();
        let prompt: Value = serde_json::from_str(&original.payload.prompt).unwrap();
        assert_eq!(
            prompt["compact_context"]["facts"].as_array().unwrap().len(),
            if different_facts { 2 } else { 1 }
        );
        assert_eq!(
            prompt["compact_context"]["evidence_options"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        for path in [
            "/compact_context/query_history/latest_query/context/fact_indexes/0",
            "/compact_context/query_history/latest_query/evidence_option_indexes/0",
        ] {
            if different_facts && path.contains("/context/fact_indexes") {
                continue;
            }
            let mut changed = original.clone();
            let mut malformed = prompt.clone();
            *malformed.pointer_mut(path).unwrap() = json!(1);
            // The loose registry bound is 2, so this index must be rejected by
            // the semantic sealer's actual-array bound rather than that bound.
            let context: ProgressiveInvestigationContext =
                serde_json::from_value(malformed["compact_context"].clone()).unwrap();
            context
                .query_history
                .validate_registry(
                    malformed["completion_contract"]["request_hash"]
                        .as_str()
                        .unwrap(),
                    input.ledger(),
                    policy,
                )
                .unwrap();
            changed.payload.prompt = malformed.to_string();
            let prepared = firewall
                .prepare_bounded(&provider, &changed, &CompletionLimits::default())
                .unwrap();
            assert!(prepared.matches_action(&changed));
            assert_eq!(prepared.preview().egress.redaction_count, 0);
            assert_eq!(
                seal_with_policy(
                    input.ledger(),
                    &changed.payload,
                    &prepared,
                    policy,
                    Some(std::slice::from_ref(&target))
                ),
                Err(InvestigationError::InvalidInput),
                "{path}"
            );
        }
    }
}
