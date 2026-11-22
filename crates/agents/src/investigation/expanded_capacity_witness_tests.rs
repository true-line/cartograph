//! A reachable overflow witness, never a full-budget fit proof. No provider,
//! filesystem capture, or Vendure preparation is involved in this diagnostic.
use super::*;
use context_hub::{
    ContextSnapshot, QueryRequest, QueryScope,
    coverage::{CoverageContext, CoverageQualification, QualifiedRequest, ScopedRepo},
};
use core_graph::coverage::{CoverageRecord, StoredCoverage};
use core_prov::content_hash;
use serde_json::{Value, json};
use std::sync::Arc;

fn fixture() -> (
    CoverageContext,
    InvestigationInputLedger,
    StagedObligationRequest,
) {
    let snapshot = Arc::new(ContextSnapshot::new(vec![], vec![]).unwrap());
    let record = CoverageRecord {
        repo_key: "capacity/empty".into(),
        registered_source_id: Some("src:capacity".into()),
        ingest_id: "capacity-fixture".into(),
        config_hash: "capacity-config".into(),
        inventory_json: json!({"inventory":"coverage@1", "repo":"capacity/empty",
            "commit":"a".repeat(40), "layer_hints":[], "producers":[],
            "unsupported":[], "limited":false})
        .to_string(),
    };
    let stored = StoredCoverage {
        inventory_hash: record.inventory_hash(),
        record,
    };
    let qualification = CoverageQualification::new(
        &[ScopedRepo {
            repo_key: stored.record.repo_key.clone(),
            registered_source_id: stored.record.registered_source_id.clone(),
            config_hash: stored.record.config_hash.clone(),
            layer_hints: None,
        }],
        std::slice::from_ref(&stored),
    )
    .unwrap();
    let context = CoverageContext::from_shared_snapshot(snapshot, qualification).unwrap();
    let ledger = InvestigationInputLedger {
        schema_version: 2,
        coverage: Some(context.durable_proof().unwrap()),
        coverage_reads: vec![],
        artifact_reads: vec![],
        graph_snapshot_id: context.snapshot_id().into(),
        scope_snapshot_id: context.snapshot_id().into(),
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    let mut obligation = Obligation::Inventory {
        obligation_id: String::new(),
        target: InventoryTarget {
            context_id: context.context_id().into(),
            repo: stored.record.repo_key,
            inventory_hash: stored.inventory_hash,
        },
    };
    let id = obligation.content_id().unwrap();
    if let Obligation::Inventory { obligation_id, .. } = &mut obligation {
        *obligation_id = id;
    }
    let request = StagedObligationRequest {
        schema_version: 5,
        request_nonce: "expanded-capacity-witness".into(),
        specialist_id: "domain-analyst@8".into(),
        obligations: ObligationEnvelope {
            schema_version: 2,
            contract: "investigation-completion-obligations@2".into(),
            obligations: vec![obligation],
        },
        inspection_requirements: None,
        question: "Describe available evidence.".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: INVESTIGATION_V2_POLICY.profile().into(),
        expected_graph_revision: Some(ledger.graph_snapshot_id.clone()),
        expected_context_id: context.context_id().into(),
        conversation_id: None,
        parent_id: None,
    }
    .normalized_with_policy(&INVESTIGATION_V2_POLICY)
    .unwrap();
    request
        .validate_ledger_with_policy(&ledger, &INVESTIGATION_V2_POLICY)
        .unwrap();
    (context, ledger, request)
}

#[test]
fn expanded_capacity_reachable_query_prefix_exceeds_transport_before_v2_budgets() {
    // AC-0281: the expanded aggregate ceilings do not promise that every
    // permitted next prompt fits the unchanged per-call transport boundary.
    query_prefix_overflow(4);
    query_prefix_overflow(16);
}

fn query_prefix_overflow(label_count: usize) {
    let policy = &INVESTIGATION_V2_POLICY;
    let limits = policy.limits();
    let (context, mut ledger, request) = fixture();
    let metadata = staged_refusal::ProtectedCompletionMetadata {
        schema_version: 1,
        request_hash: request.intent_hash_with_policy(policy).unwrap(),
        obligations_hash: request.obligations.fingerprint_with_policy(policy).unwrap(),
        completed_attempt_ids: vec![],
        refusal_count: 0,
        last_refusal_id: None,
        feedback: None,
    };
    let initial_basis = InvestigationTurnBasis {
        input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
        previous_ledger: None,
        completed_tool: None,
    };
    let initial = InvestigationInput::prepare_v7_with_policy(
        &request.question,
        SpecialistId::DomainAnalystV8,
        ledger.clone(),
        &[],
        &[],
        &[],
        &InvestigationUsage::default(),
        &initial_basis,
        &request,
        &metadata,
        &[],
        policy,
    )
    .unwrap();
    let mut diagnostic_payload = initial
        .action("inv:capacity-witness".into())
        .unwrap()
        .payload;
    let mut diagnostic_prompt: Value = serde_json::from_str(&diagnostic_payload.prompt).unwrap();
    let query = InvestigationQuery {
        scope: InvestigationScope::All,
        kind: None,
        // The provider is allowed these exact, repeated, nonempty labels. The
        // query service normalizes them internally; the prompt retains the
        // original admitted selection and its escaped bytes on every page.
        labels: vec!["\\".repeat(128); label_count],
        max_facts: MAX_QUERY_FACTS,
        max_bytes: MAX_QUERY_BYTES,
        cursor: None,
    };
    query.validate().unwrap();
    let selector_bytes = serde_json::to_vec(&query).unwrap().len();
    let action = json!({"type":"query_context", "context_id":context.context_id(), "query":query})
        .to_string();
    assert!(action.len() <= MAX_ACTION_BYTES);
    if label_count == 4 {
        // This variant has a complete action below 2048 UTF-8 bytes. This is
        // structural capacity evidence, not observed model output or a claim
        // about the configured provider's tokenizer or semantic behavior.
        assert!(action.len() < limits.generated_tokens_per_invocation as usize);
    }
    assert!(
        matches!(CompletionInvestigationAction::decode(&action, context.context_id()).unwrap(),
        CompletionInvestigationAction::Read(QualifiedInvestigationAction::QueryContext { query: admitted, .. }) if admitted == query)
    );
    let mut pages = Vec::new();
    let mut last_fitting_size = serde_json::to_vec(&diagnostic_payload).unwrap().len();
    for count in 1u32..limits.tool_actions {
        // These are real normalized service results, not invented empty pages.
        let response = context
            .query(QualifiedRequest {
                request: QueryRequest {
                    scope: QueryScope::All,
                    kind: None,
                    labels: query.labels.clone(),
                    max_facts: query.max_facts,
                    max_bytes: query.max_bytes,
                    cursor: None,
                },
                context_id: Some(context.context_id().into()),
            })
            .unwrap();
        assert!(response.response.facts.is_empty());
        assert_eq!(response.response.total_selected, 0);
        assert!(response.response.next_cursor.is_none());
        let raw = json!({"context":response,"evidence_options":[]}).to_string();
        assert!(raw.len() <= query.max_bytes);
        let previous = ledger.clone();
        ledger.revision += 1;
        ledger.queries.push(InvestigationQueryManifest {
            query: query.clone(),
            response_hash: content_hash(raw.as_bytes()),
            response_bytes: raw.len(),
            returned_facts: 0,
            total_selected: 0,
            has_more: false,
        });
        pages.push(raw);
        request
            .validate_ledger_with_policy(&ledger, policy)
            .unwrap();
        let query_budget = InvestigationQueryBudget::from_ledger(&ledger).unwrap();
        assert!(query_budget.consumed_bytes < MAX_TOTAL_QUERY_BYTES);
        assert!(ledger.selected_facts.is_empty());
        assert!(ledger.citations.is_empty());
        assert!(count < limits.tool_actions);
        assert!(count + 1 < limits.model_invocations);
        let usage = InvestigationUsage {
            model_invocations: count,
            tool_actions: count,
            generated_token_reservations: count * limits.generated_tokens_per_invocation,
            ..InvestigationUsage::default()
        };
        assert!(
            usage.generated_token_reservations + limits.generated_tokens_per_invocation
                <= limits.generated_token_reservations
        );
        let basis = InvestigationTurnBasis {
            input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
            previous_ledger: Some(previous),
            completed_tool: Some(InvestigationTool::QueryContext),
        };
        basis.validate_with_policy(&ledger, policy).unwrap();
        let compact = serde_json::to_value(
            CompactInvestigationContext::prepare_with_policy(&ledger, &pages, policy).unwrap(),
        )
        .unwrap();
        let progress = basis
            .progress_with_policy(&ledger, &compact, policy)
            .unwrap();
        diagnostic_prompt["compact_context"] = compact;
        diagnostic_prompt["input_ledger_hash"] = basis.input_ledger_hash.clone().into();
        diagnostic_prompt["turn_progress"] = serde_json::to_value(progress).unwrap();
        diagnostic_prompt["consumed_budgets"] = serde_json::to_value(&usage).unwrap();
        diagnostic_payload.prompt = serde_json::to_string(&diagnostic_prompt).unwrap();
        let diagnostic_bytes = serde_json::to_vec(&diagnostic_payload).unwrap();
        let prepared = InvestigationInput::prepare_v7_with_policy(
            &request.question,
            SpecialistId::DomainAnalystV8,
            ledger.clone(),
            &pages,
            &[],
            &[],
            &usage,
            &basis,
            &request,
            &metadata,
            &[],
            policy,
        );
        if diagnostic_bytes.len() <= MAX_INPUT_BYTES {
            // Check the uncapped diagnostic against the full production builder
            // at every fitting prefix before using it to explain the rejection.
            let actual = prepared
                .unwrap()
                .action("inv:capacity-witness".into())
                .unwrap();
            assert_eq!(
                serde_json::to_vec(&actual.payload).unwrap(),
                diagnostic_bytes
            );
            last_fitting_size = diagnostic_bytes.len();
        } else {
            assert!(matches!(prepared, Err(InvestigationError::LimitExceeded)));
            assert!(diagnostic_payload.prompt.len() < MAX_INPUT_BYTES);
            eprintln!(
                "expanded capacity structurally admitted witness: labels={label_count}, action_bytes={}, queries={count}, next_call={}, previous_payload={last_fitting_size}, prompt={}, payload={}, ledger={}, raw_query_bytes={}, selector_bytes_each={selector_bytes}, cumulative_selector_bytes={}, spans=0, attempts=0, feedback=false, history=0, cap={MAX_INPUT_BYTES}",
                action.len(),
                count + 1,
                diagnostic_payload.prompt.len(),
                diagnostic_bytes.len(),
                serde_json::to_vec(&ledger).unwrap().len(),
                query_budget.consumed_bytes,
                usize::try_from(count).unwrap() * selector_bytes,
            );
            return;
        }
    }
    panic!(
        "The preregistered query-prefix witness no longer overflows; reassess the runtime projection and capacity proof."
    );
}

#[test]
fn query_history_v9_builder_fits_all_admitted_empty_query_depths() {
    // AC-0286: production prompt8 construction fits these empty-page scenarios
    // through every V2 tool depth, including unique escaped selectors. This is
    // not a proof for arbitrary source, inventory, attempts or parent history.
    let policy = &INVESTIGATION_V2_POLICY;
    let limits = policy.limits();
    for distinct in [false, true] {
        let (context, mut ledger, mut request) = fixture();
        request.schema_version = 6;
        request.specialist_id = "domain-analyst@9".into();
        request.request_nonce.push_str("-v9");
        let request = request.normalized_with_policy(policy).unwrap();
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
        let mut maximum_payload = 0;
        let mut previous = None;
        for count in 0..=limits.tool_actions {
            if count != 0 {
                previous = Some(ledger.clone());
                let suffix = if distinct {
                    format!("{count:02}")
                } else {
                    "xx".into()
                };
                let query = InvestigationQuery {
                    scope: InvestigationScope::All,
                    kind: None,
                    labels: vec![format!("{}{}", "\\".repeat(126), suffix); 4],
                    max_facts: MAX_QUERY_FACTS,
                    max_bytes: MAX_QUERY_BYTES,
                    cursor: None,
                };
                query.validate().unwrap();
                let response = context
                    .query(QualifiedRequest {
                        request: QueryRequest {
                            scope: QueryScope::All,
                            kind: None,
                            labels: query.labels.clone(),
                            max_facts: query.max_facts,
                            max_bytes: query.max_bytes,
                            cursor: None,
                        },
                        context_id: Some(context.context_id().into()),
                    })
                    .unwrap();
                assert!(response.response.facts.is_empty());
                let raw = json!({"context":response,"evidence_options":[]}).to_string();
                ledger.revision += 1;
                ledger.queries.push(InvestigationQueryManifest {
                    query,
                    response_hash: content_hash(raw.as_bytes()),
                    response_bytes: raw.len(),
                    returned_facts: 0,
                    total_selected: 0,
                    has_more: false,
                });
                pages.push(raw);
            }
            ledger.validate_with_policy(policy).unwrap();
            let basis = InvestigationTurnBasis {
                input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
                previous_ledger: previous.clone(),
                completed_tool: (count != 0).then_some(InvestigationTool::QueryContext),
            };
            let usage = InvestigationUsage {
                model_invocations: count,
                tool_actions: count,
                generated_token_reservations: count * limits.generated_tokens_per_invocation,
                ..InvestigationUsage::default()
            };
            let input = InvestigationInput::prepare_v8_with_policy(
                &request.question,
                SpecialistId::DomainAnalystV9,
                ledger.clone(),
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
            if matches!(count, 0 | 2 | 56) {
                verify_progressive_sealing(&input, &ledger, &request);
            }
            let payload = input
                .action("inv:query-history-witness".into())
                .unwrap()
                .payload;
            let bytes = serde_json::to_vec(&payload).unwrap().len();
            assert!(bytes <= MAX_INPUT_BYTES);
            maximum_payload = maximum_payload.max(bytes);
            let prompt: Value = serde_json::from_str(&payload.prompt).unwrap();
            assert_eq!(prompt["schema_version"], 8);
            assert!(prompt["compact_context"].get("queries").is_none());
            if count != 0 {
                assert!(
                    prompt["turn_progress"]["last_completed_action"]
                        .get("query")
                        .is_none()
                );
                assert_eq!(
                    prompt["turn_progress"]["last_completed_action"]["query_id"],
                    prompt["compact_context"]["query_history"]["latest_query"]["reference"]["query_id"]
                );
            }
        }
        eprintln!(
            "query history production prompt8 empty-page witness: distinct={distinct}, depths=0..{}, maximum_payload={maximum_payload}, raw_query_bytes={}, ledger={}",
            limits.tool_actions,
            InvestigationQueryBudget::from_ledger(&ledger)
                .unwrap()
                .consumed_bytes,
            serde_json::to_vec(&ledger).unwrap().len()
        );
    }
}

// AC-0286: local firewall preparation only; no provider dispatch.
fn verify_progressive_sealing(
    input: &InvestigationInput,
    ledger: &InvestigationInputLedger,
    request: &StagedObligationRequest,
) {
    use llm::{EgressFirewall, EgressPolicy, OllamaProvider, bounded::CompletionLimits};
    let policy = &INVESTIGATION_V2_POLICY;
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let original = input.action("inv:query-history-seal".into()).unwrap();
    let prepared = firewall
        .prepare_bounded(&provider, &original, &CompletionLimits::default())
        .unwrap();
    let disclosure = input
        .seal_disclosure_with_policy(&prepared, policy)
        .unwrap()
        .unwrap();
    assert_eq!(disclosure.schema_version, 5);
    assert_eq!(
        disclosure.query_history.as_ref().unwrap().request_hash,
        request.intent_hash_with_policy(policy).unwrap()
    );
    assert!(
        disclosure
            .eligible_citations_with_policy(ledger, policy)
            .unwrap()
            .is_empty()
    );
    assert!(
        disclosure
            .fingerprint_with_policy(ledger, policy)
            .unwrap()
            .starts_with("investigation-disclosure-v5:")
    );
    for (index, query) in disclosure.queries.iter().enumerate() {
        assert_eq!(
            query.representation.is_supplied(),
            index + 1 == ledger.queries.len()
        );
    }
    if ledger.queries.len() < 2 {
        return;
    }
    let request_hash = request.intent_hash_with_policy(policy).unwrap();
    let reference = QueryHistoryReference::from_ledger(&request_hash, ledger, 0, policy).unwrap();
    let context_id = &ledger.coverage.as_ref().unwrap().context_id;
    let replay = json!({"type":"replay_query", "context_id":context_id, "query_index":0, "query_id":reference.query_id});
    let decoded = ProgressiveInvestigationAction::decode_resolved(
        &replay.to_string(),
        context_id,
        &request_hash,
        ledger,
        policy,
    )
    .unwrap();
    let CompletionInvestigationAction::Read(QualifiedInvestigationAction::QueryContext {
        query,
        ..
    }) = decoded
    else {
        panic!("replay did not resolve to an ordinary query")
    };
    assert_eq!(query, ledger.queries[0].query);
    assert!(CompletionInvestigationAction::decode(&replay.to_string(), context_id).is_err());
    for (key, value) in [
        ("context_id", json!("wrong")),
        ("query_index", json!(1)),
        ("query_id", json!("a".repeat(64))),
        ("extra", json!(true)),
    ] {
        let mut invalid = replay.clone();
        invalid[key] = value;
        assert!(
            ProgressiveInvestigationAction::decode_resolved(
                &invalid.to_string(),
                context_id,
                &request_hash,
                ledger,
                policy
            )
            .is_err()
        );
    }
    assert!(
        ProgressiveInvestigationAction::decode_resolved(
            &replay.to_string(),
            context_id,
            &"a".repeat(64),
            ledger,
            policy
        )
        .is_err()
    );
    let prompt: Value = serde_json::from_str(&original.payload.prompt).unwrap();
    for path in [
        "/compact_context/query_history/registry_hash",
        "/compact_context/query_history/prior_queries/0/query_id",
        "/compact_context/query_history/latest_query/reference/query_id",
        "/compact_context/query_history/latest_query/context/next_cursor",
        "/compact_context/query_history/latest_query/selection/labels/0",
        "/turn_progress/last_completed_action/query_id",
    ] {
        let mut changed = original.clone();
        let mut mutated = prompt.clone();
        *mutated.pointer_mut(path).unwrap() = json!("changed");
        changed.payload.prompt = mutated.to_string();
        let prepared = firewall
            .prepare_bounded(&provider, &changed, &CompletionLimits::default())
            .unwrap();
        assert!(
            input
                .seal_disclosure_with_policy(&prepared, policy)
                .is_err(),
            "accepted mutation at {path}"
        );
        // Exercise the inner semantic checks, independently of the outer
        // prepared.matches_action identity rejection.
        assert!(
            super::sealing::seal_with_policy(ledger, &original.payload, &prepared, policy, None,)
                .is_err(),
            "inner seal accepted mutation at {path}"
        );
    }
    let mut changed = disclosure.clone();
    changed.queries[0].representation = DisclosureRepresentation::Supplied {
        representation_hash: "a".repeat(64),
    };
    assert!(changed.validate_with_policy(ledger, policy).is_err());
}

#[test]
fn query_history_firewall_redaction_rejects_latest_selector_after_action_match() {
    // AC-0286: an actual firewall rewrite must fail the semantic history seal,
    // independently of the outer original-action identity check.
    use llm::{EgressFirewall, EgressPolicy, OllamaProvider, bounded::CompletionLimits};
    let policy = &INVESTIGATION_V2_POLICY;
    let (context, mut ledger, mut request) = fixture();
    request.schema_version = 6;
    request.specialist_id = "domain-analyst@9".into();
    request.request_nonce.push_str("-selector-redaction");
    let request = request.normalized_with_policy(policy).unwrap();
    let metadata = staged_refusal::ProtectedCompletionMetadata {
        schema_version: 1,
        request_hash: request.intent_hash_with_policy(policy).unwrap(),
        obligations_hash: request.obligations.fingerprint_with_policy(policy).unwrap(),
        completed_attempt_ids: vec![],
        refusal_count: 0,
        last_refusal_id: None,
        feedback: None,
    };
    let previous = ledger.clone();
    let query = InvestigationQuery {
        scope: InvestigationScope::All,
        kind: None,
        labels: vec!["password=supersecret123".into()],
        max_facts: MAX_QUERY_FACTS,
        max_bytes: MAX_QUERY_BYTES,
        cursor: None,
    };
    query.validate().unwrap();
    let response = context
        .query(QualifiedRequest {
            request: QueryRequest {
                scope: QueryScope::All,
                kind: None,
                labels: query.labels.clone(),
                max_facts: query.max_facts,
                max_bytes: query.max_bytes,
                cursor: None,
            },
            context_id: Some(context.context_id().into()),
        })
        .unwrap();
    assert!(response.response.facts.is_empty());
    let raw = json!({"context":response,"evidence_options":[]}).to_string();
    ledger.revision += 1;
    ledger.queries.push(InvestigationQueryManifest {
        query,
        response_hash: content_hash(raw.as_bytes()),
        response_bytes: raw.len(),
        returned_facts: 0,
        total_selected: 0,
        has_more: false,
    });
    let basis = InvestigationTurnBasis {
        input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
        previous_ledger: Some(previous),
        completed_tool: Some(InvestigationTool::QueryContext),
    };
    let input = InvestigationInput::prepare_v8_with_policy(
        &request.question,
        SpecialistId::DomainAnalystV9,
        ledger.clone(),
        &[raw],
        &[],
        &[],
        &InvestigationUsage {
            model_invocations: 1,
            tool_actions: 1,
            generated_token_reservations: policy.limits().generated_tokens_per_invocation,
            ..InvestigationUsage::default()
        },
        &basis,
        &request,
        &metadata,
        &[],
        policy,
    )
    .unwrap();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let original = input.action("inv:selector-redaction".into()).unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let prepared = firewall
        .prepare_bounded(&provider, &original, &CompletionLimits::default())
        .unwrap();
    assert!(prepared.matches_action(&original));
    assert_eq!(prepared.preview().egress.redaction_count, 1);
    let before: Value = serde_json::from_str(&original.payload.prompt).unwrap();
    let after: Value = serde_json::from_str(&prepared.preview().egress.payload.prompt).unwrap();
    let mut expected = before.clone();
    expected["compact_context"]["query_history"]["latest_query"]["selection"]["labels"][0] =
        json!("password=[REDACTED]");
    // The only changed prompt value is the protected latest selector; hashes,
    // contract, metadata, qualification, progress and citation fields survive.
    assert_eq!(after, expected);
    assert_ne!(
        before["compact_context"]["query_history"],
        after["compact_context"]["query_history"]
    );
    assert!(
        super::sealing::seal_with_policy(&ledger, &original.payload, &prepared, policy, None,)
            .is_err()
    );
    assert!(
        input
            .seal_disclosure_with_policy(&prepared, policy)
            .is_err()
    );
}

// AC-0430, AC-0436: original errors and accepted actions survive detailed attribution.
#[test]
fn action_decode_stages_preserve_legacy_results_without_reflecting_content() {
    use ActionDecodeStage as Stage;
    let (_, mut ledger, request) = fixture();
    let policy = &INVESTIGATION_V2_POLICY;
    let context = ledger.coverage.as_ref().unwrap().context_id.clone();
    let hash = request.intent_hash_with_policy(policy).unwrap();
    let query = json!({"type":"query_context","context_id":context,"query":{"scope":{"type":"all"},"kind":null,"labels":[],"max_facts":1,"max_bytes":1024,"cursor":null}});
    ledger.revision += 1;
    ledger.queries.push(InvestigationQueryManifest {
        query: serde_json::from_value(query["query"].clone()).unwrap(),
        response_hash: content_hash(b"retained query response"),
        response_bytes: 23,
        returned_facts: 0,
        total_selected: 0,
        has_more: false,
    });
    let reference = QueryHistoryReference::from_ledger(&hash, &ledger, 0, policy).unwrap();
    let decode = |raw: &str| {
        ProgressiveInvestigationAction::decode_resolved_detailed(
            raw, &context, &hash, &ledger, policy,
        )
    };
    for valid in [
        query.clone(),
        json!({"type":"read_coverage_inventory","context_id":context,"repo":"repo","inventory_hash":"a".repeat(64),"max_bytes":1}),
        json!({"type":"read_evidence","fact":{"kind":"node","id":"node:test"},"role":"provenance","index":0}),
        json!({"schema_version":1,"outcome":"insufficient_evidence","action":{"type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence","limitations":["Missing evidence."]}}),
        json!({"type":"replay_query","context_id":context,"query_index":0,"query_id":reference.query_id}),
    ] {
        let raw = valid.to_string();
        let original =
            ProgressiveInvestigationAction::decode_resolved(&raw, &context, &hash, &ledger, policy)
                .unwrap();
        assert_eq!(
            format!("{original:?}"),
            format!("{:?}", decode(&raw).unwrap())
        );
    }
    let mut omitted = query.clone();
    omitted["query"].as_object_mut().unwrap().remove("kind");
    let mut wrong_context = query;
    wrong_context["context_id"] = json!("PRIVATE-REJECTED-1281");
    for (raw, stage, error) in [
        (
            "PRIVATE-REJECTED-1281".into(),
            Stage::Preflight,
            InvestigationError::InvalidInput,
        ),
        (
            json!({"type":"PRIVATE-REJECTED-1281"}).to_string(),
            Stage::TypedLegacyAction,
            InvestigationError::InvalidAction,
        ),
        (
            omitted.to_string(),
            Stage::CanonicalFields,
            InvestigationError::InvalidAction,
        ),
        (
            wrong_context.to_string(),
            Stage::SemanticRule(ActionSemanticRule::QueryContext),
            InvestigationError::InvalidAction,
        ),
        (
            "x".repeat(MAX_ACTION_BYTES + 1),
            Stage::Preflight,
            InvestigationError::LimitExceeded,
        ),
    ] {
        let failure = decode(&raw).unwrap_err();
        assert_eq!(failure, ActionDecodeFailure { stage, error });
        assert_eq!(
            ProgressiveInvestigationAction::decode_resolved(&raw, &context, &hash, &ledger, policy)
                .unwrap_err(),
            error
        );
        assert!(!format!("{failure:?}").contains("PRIVATE-REJECTED-1281"));
    }
    // Pin the old public error independently of the new rule attribution.
    let finish = json!({"schema_version":1,"outcome":"insufficient_evidence","action":{"type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence","limitations":["Missing evidence."]}});
    let mut shape = finish.clone();
    shape["action"]["limitations"] = json!([]);
    let mut role = finish.clone();
    role["schema_version"] = json!(2);
    let mut outcome = finish;
    outcome["outcome"] = json!("supported");
    let mut query_shape = ledger.queries[0].query.clone();
    query_shape.max_facts = 0;
    let mut scope = query_shape.clone();
    scope.scope = InvestigationScope::Neighborhood {
        anchor: String::new(),
        hops: 1,
    };
    for (value, rule, error) in [
        (
            json!({"type":"query_context","context_id":context,"query":scope}),
            ActionSemanticRule::QueryShape,
            InvestigationError::InvalidRequest,
        ),
        (
            json!({"type":"query_context","context_id":context,"query":query_shape}),
            ActionSemanticRule::QueryShape,
            InvestigationError::InvalidAction,
        ),
        (
            json!({"type":"read_coverage_inventory","context_id":"wrong","repo":"repo","inventory_hash":"a".repeat(64),"max_bytes":1}),
            ActionSemanticRule::InventoryContext,
            InvestigationError::InvalidAction,
        ),
        (
            json!({"type":"read_coverage_inventory","context_id":context,"repo":"","inventory_hash":"a".repeat(64),"max_bytes":1}),
            ActionSemanticRule::InventoryShape,
            InvestigationError::InvalidAction,
        ),
        (
            json!({"type":"replay_query","context_id":"wrong","query_index":0,"query_id":reference.query_id}),
            ActionSemanticRule::ReplayContext,
            InvestigationError::InvalidAction,
        ),
        (
            json!({"type":"replay_query","context_id":context,"query_index":1,"query_id":reference.query_id}),
            ActionSemanticRule::ReplayReference,
            InvestigationError::InvalidAction,
        ),
        (
            json!({"type":"read_evidence","fact":{"kind":"node","id":""},"role":"provenance","index":0}),
            ActionSemanticRule::EvidenceShape,
            InvestigationError::InvalidAction,
        ),
        (
            role,
            ActionSemanticRule::FinishRole,
            InvestigationError::InvalidAction,
        ),
        (
            shape,
            ActionSemanticRule::FinishOverall,
            InvestigationError::InvalidAction,
        ),
        (
            outcome,
            ActionSemanticRule::FinishOutcome,
            InvestigationError::InvalidAction,
        ),
    ] {
        let raw = value.to_string();
        assert_eq!(
            decode(&raw).unwrap_err(),
            ActionDecodeFailure {
                stage: Stage::SemanticRule(rule),
                error
            }
        );
        assert_eq!(
            ProgressiveInvestigationAction::decode_resolved(&raw, &context, &hash, &ledger, policy)
                .unwrap_err(),
            error
        );
    }
    // Ordered predicates and nested stages retain the old public InvalidAction.
    let supported = json!({"schema_version":1,"outcome":"supported","action":{"type":"finish","findings":[{"claim_kind":"inferred_interpretation","title":"Title","statement":"Statement","citation_ids":["citation"],"limitations":["Limited."]}],"knowledge_completeness":"partial","limitations":["Limited."]}});
    assert!(decode(&supported.to_string()).is_ok());
    // Exact content returned by the bounded OpenAI loopback transport fixture.
    let duplicate_finish =
        include_str!("../../../llm/tests/fixtures/duplicate-citation-finish.json").trim();
    assert_eq!(
        decode(duplicate_finish).unwrap_err().stage,
        Stage::SemanticRule(ActionSemanticRule::FindingCitations)
    );
    assert_eq!(
        ProgressiveInvestigationAction::decode_resolved(
            duplicate_finish,
            &context,
            &hash,
            &ledger,
            policy
        )
        .unwrap_err(),
        InvestigationError::InvalidAction
    );
    for (pointer, value, stage) in [
        (
            "/action/limitations",
            json!([]),
            Stage::SemanticRule(ActionSemanticRule::FinishOverall),
        ),
        (
            "/action/findings/0/title",
            json!(""),
            Stage::SemanticRule(ActionSemanticRule::FindingTextLimits),
        ),
        (
            "/action/findings/0/limitations",
            json!([]),
            Stage::SemanticRule(ActionSemanticRule::FindingTextLimits),
        ),
        (
            "/action/findings/0/citation_ids",
            json!(["citation", "citation"]),
            Stage::SemanticRule(ActionSemanticRule::FindingCitations),
        ),
        (
            "/action/findings/0/title",
            json!(1),
            Stage::TypedStagedFinish,
        ),
    ] {
        let mut value_under_test = supported.clone();
        *value_under_test.pointer_mut(pointer).unwrap() = value;
        let raw = value_under_test.to_string();
        assert_eq!(
            decode(&raw).unwrap_err(),
            ActionDecodeFailure {
                stage,
                error: InvestigationError::InvalidAction
            }
        );
        assert_eq!(
            ProgressiveInvestigationAction::decode_resolved(&raw, &context, &hash, &ledger, policy)
                .unwrap_err(),
            InvestigationError::InvalidAction
        );
    }
    let mut ordered = supported.clone();
    ordered["action"]["findings"][0]["citation_ids"] = json!([]);
    ordered["action"]["findings"][0]["limitations"] = json!([]);
    assert_eq!(
        decode(&ordered.to_string()).unwrap_err().stage,
        Stage::SemanticRule(ActionSemanticRule::FindingCitations)
    );
    ordered["action"]["findings"][0]["title"] = json!("");
    assert_eq!(
        decode(&ordered.to_string()).unwrap_err().stage,
        Stage::SemanticRule(ActionSemanticRule::FindingTextLimits)
    );
    ordered["action"]["limitations"] = json!([]);
    assert_eq!(
        decode(&ordered.to_string()).unwrap_err().stage,
        Stage::SemanticRule(ActionSemanticRule::FinishOverall)
    );
    let nested = json!({"schema_version":1,"outcome":"supported","action":{"type":"read_evidence","fact":{"kind":"node","id":""},"role":"provenance","index":0}});
    assert_eq!(
        decode(&nested.to_string()).unwrap_err().stage,
        Stage::SemanticRule(ActionSemanticRule::EvidenceShape)
    );
    // Inner typed serialization normalizes omitted query.kind before decoding;
    // the unchanged role check precedes the outer canonical check.
    let omitted = json!({"schema_version":1,"outcome":"supported","action":{"type":"query_context","query":{"scope":{"type":"all"},"labels":[],"max_facts":1,"max_bytes":1024,"cursor":null}}});
    assert_eq!(
        decode(&omitted.to_string()).unwrap_err().stage,
        Stage::SemanticRule(ActionSemanticRule::FinishRole)
    );
    let mut noncanonical = supported.clone();
    noncanonical["narrative"] = serde_json::Value::Null;
    assert_eq!(
        decode(&noncanonical.to_string()).unwrap_err().stage,
        Stage::CanonicalFields
    );
    let mut missing = supported.clone();
    missing["action"]["findings"][0]
        .as_object_mut()
        .unwrap()
        .remove("claim_kind");
    for (value, stage) in [
        (missing, Stage::TypedStagedFinish),
        (supported["action"].clone(), Stage::BareFinish),
        (
            json!({"type":"read_coverage_inventory","context_id":context,"repo":1,"inventory_hash":"a".repeat(64),"max_bytes":1}),
            Stage::TypedCoverageRead,
        ),
        (
            json!({"type":"read_evidence","fact":{"kind":"node","id":"node:test"},"role":"provenance","index":"0"}),
            Stage::TypedLegacyAction,
        ),
        (
            json!({"type":"replay_query","context_id":context,"query_index":"0","query_id":reference.query_id}),
            Stage::TypedReplay,
        ),
    ] {
        let raw = value.to_string();
        assert_eq!(
            decode(&raw).unwrap_err(),
            ActionDecodeFailure {
                stage,
                error: InvestigationError::InvalidAction
            }
        );
        assert_eq!(
            ProgressiveInvestigationAction::decode_resolved(&raw, &context, &hash, &ledger, policy)
                .unwrap_err(),
            InvestigationError::InvalidAction
        );
    }
    let host = ProgressiveInvestigationAction::decode_resolved_detailed(
        "{}", "wrong", &hash, &ledger, policy,
    )
    .unwrap_err();
    assert_eq!(host.stage, Stage::HostBinding);
    assert_eq!(host.error, InvestigationError::InvalidAction);
}
