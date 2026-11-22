use super::*;
use crate::{TaskFactKey, TaskFactSelection};
use core_prov::{ConfidenceTier, Tier, content_hash};
use serde_json::json;
use std::collections::BTreeSet;

pub(super) fn request() -> StartInvestigationRequest {
    StartInvestigationRequest {
        inspection_requirements: None,
        schema_version: 1,
        request_nonce: "request-1".into(),
        specialist_id: SpecialistId::DomainAnalyst,
        question: "What governs checkout?".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: "investigation-v1".into(),
        expected_graph_revision: None,
        expected_context_id: None,
        conversation_id: None,
        parent_id: None,
    }
}
fn query() -> InvestigationQuery {
    InvestigationQuery {
        scope: InvestigationScope::All,
        kind: None,
        labels: vec![],
        max_facts: 12,
        max_bytes: 16384,
        cursor: None,
    }
}
fn action() -> InvestigationAction {
    InvestigationAction::Finish {
        findings: vec![ProposedInvestigationFinding {
            claim_kind: InvestigationClaimKind::ImplementedBehavior,
            title: "Checkout guard".into(),
            statement: "The transition has a rejection path.".into(),
            citation_ids: vec!["fact-1".into()],
            limitations: vec!["Other routes were not assessed.".into()],
        }],
        knowledge_completeness: KnowledgeCompleteness::Partial,
        limitations: vec!["The investigation covers selected evidence only.".into()],
    }
}
pub(super) fn ledger() -> InvestigationInputLedger {
    let fact = TaskFactKey::Node {
        id: "node:checkout".into(),
    };
    let digest = format!("node-v1:{}", "a".repeat(64));
    InvestigationInputLedger {
        coverage_reads: vec![],
        artifact_reads: vec![],
        schema_version: 1,
        coverage: None,
        graph_snapshot_id: format!("context-v1:{}", "b".repeat(64)),
        scope_snapshot_id: format!("context-v1:{}", "b".repeat(64)),
        revision: 1,
        selected_facts: vec![TaskFactSelection {
            fact: fact.clone(),
            fact_digest: digest.clone(),
            binding: None,
        }],
        receipt_references: vec![],
        citations: vec![InvestigationCitation {
            citation_id: "fact-1".into(),
            fact,
            fact_digest: digest,
            source: None,
            role: None,
            index: None,
            text_hash: None,
            origin: InvestigationEvidenceOrigin::GraphMetadata,
        }],
        queries: vec![InvestigationQueryManifest {
            query: query(),
            response_hash: content_hash(b"test-query-result"),
            response_bytes: 200,
            returned_facts: 1,
            total_selected: 1,
            has_more: false,
        }],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    }
}

#[test]
fn investigation_specialists_are_versioned_distinct_and_propose_only() {
    // AC-0180: changing specialist identity changes the exact immutable prompt basis.
    let analyst = SpecialistId::DomainAnalyst.definition().unwrap();
    let auditor = SpecialistId::EvidenceAuditor.definition().unwrap();
    assert_ne!(analyst.prompt_fingerprint, auditor.prompt_fingerprint);
    assert_eq!(analyst, SpecialistId::DomainAnalyst.definition().unwrap());
    for specialist in [analyst, auditor] {
        assert_eq!(specialist.tier, Tier::Agentic);
        assert_eq!(specialist.confidence_tier, ConfidenceTier::InferredWeak);
        assert_eq!(
            specialist.operations,
            ["query_context", "read_evidence", "finish"]
        );
        assert_eq!(specialist.version, 2);
    }
}

#[test]
fn investigation_discovery_revision_preserves_original_specialist_identity() {
    // AC-0180: the first real-provider artifact must remain readable after a
    // prompt improvement; old IDs must not acquire a different fingerprint.
    let old = SpecialistId::DomainAnalystV1.definition().unwrap();
    assert_eq!(old.version, 1);
    assert_eq!(
        old.prompt_fingerprint,
        "ab952c9fe055e1fde7aa5ca3f1d7f60851b402021460fdbd9011a765e20967c3"
    );
    for (legacy, current, old_id, new_id) in [
        (
            SpecialistId::DomainAnalystV1,
            SpecialistId::DomainAnalyst,
            "domain-analyst@1",
            "domain-analyst@2",
        ),
        (
            SpecialistId::EvidenceAuditorV1,
            SpecialistId::EvidenceAuditor,
            "evidence-auditor@1",
            "evidence-auditor@2",
        ),
    ] {
        assert_eq!(serde_json::to_value(legacy).unwrap(), old_id);
        assert_eq!(serde_json::to_value(current).unwrap(), new_id);
        let restored: SpecialistId = serde_json::from_value(serde_json::json!(old_id)).unwrap();
        assert_eq!(restored.definition().unwrap(), legacy.definition().unwrap());
        assert_ne!(
            legacy.definition().unwrap().prompt_fingerprint,
            current.definition().unwrap().prompt_fingerprint
        );
        assert!(
            !legacy
                .prompt()
                .contains("Empty initial input is not an empty search result")
        );
        assert!(
            current
                .prompt()
                .contains("Empty initial input is not an empty search result")
        );
    }
}

#[test]
fn investigation_intent_identity_binds_original_text_before_redaction() {
    // AC-0181/0188: two secrets that redact equally must not deduplicate as one request.
    let mut first = request();
    first.question = "Explain password=first-synthetic-value".into();
    let mut second = first.clone();
    second.question = "Explain password=second-synthetic-value".into();
    assert_eq!(
        redacted_history_text(&first.question, 2048).unwrap(),
        redacted_history_text(&second.question, 2048).unwrap()
    );
    assert_ne!(first.intent_hash().unwrap(), second.intent_hash().unwrap());
    assert_eq!(
        first.intent_hash().unwrap(),
        first.clone().intent_hash().unwrap()
    );
    second.scope = InvestigationScope::Neighborhood {
        anchor: "node:checkout".into(),
        hops: 0,
    };
    assert!(second.validate().is_err());
    second = request();
    second.parent_id = Some("unlinked-parent".into());
    assert!(second.validate().is_err());
}

#[test]
fn investigation_provider_transport_identity_preserves_legacy_serialization() {
    // AC-0185: older stored request/provider hashes must not acquire a current
    // transport identity or change canonical bytes merely because they are read.
    let original = r#"{"mode":"local","provider_id":"ollama:qwen3:8b","model":"qwen3:8b","endpoint":"http://127.0.0.1:11435/api/chat","deployment":null,"available":true,"unavailable_reason":null}"#;
    let mut provider: InvestigationProvider = decode_record(original).unwrap();
    assert_eq!(provider.protocol_version, None);
    assert_eq!(serde_json::to_string(&provider).unwrap(), original);
    provider.protocol_version = Some(llm::bounded::PROTOCOL_VERSION.into());
    let current = serde_json::to_string(&provider).unwrap();
    assert_ne!(current, original);
    assert_eq!(
        decode_record::<InvestigationProvider>(&current).unwrap(),
        provider
    );
    assert_eq!(
        provider.protocol_version.as_deref(),
        Some("bounded-json-completion@2")
    );
}

#[test]
fn investigation_action_decoder_is_closed_and_never_extracts_or_repairs() {
    // AC-0184: untrusted objects never acquire an arbitrary tool or authority field.
    let valid = serde_json::to_string(&action()).unwrap();
    assert_eq!(InvestigationAction::decode(&valid).unwrap(), action());
    for raw in [format!("```json\n{valid}\n```"), format!("{valid} {{}}"),
        "{\"type\":\"run_command\",\"command\":\"anything\"}".into(),
        "{\"type\":\"finish\",\"type\":\"finish\",\"findings\":[],\"knowledge_completeness\":\"insufficient_evidence\",\"limitations\":[\"Unknown\"]}".into(),
    ] { assert!(InvestigationAction::decode(&raw).is_err()); }
    let mut value = serde_json::to_value(action()).unwrap();
    value["findings"][0]["tier"] = json!("Deterministic");
    assert!(InvestigationAction::decode(&value.to_string()).is_err());
    value = serde_json::to_value(InvestigationAction::QueryContext { query: query() }).unwrap();
    value["query"].as_object_mut().unwrap().remove("cursor");
    assert!(InvestigationAction::decode(&value.to_string()).is_err());
    value["query"]["cursor"] = serde_json::Value::Null;
    value["query"]["max_facts"] = json!(33);
    assert!(InvestigationAction::decode(&value.to_string()).is_err());
}

#[test]
fn investigation_finish_rejects_invented_citations_and_never_upgrades_claims() {
    // AC-0180/0184: even an implemented-behavior claim remains weak T3.
    let available = BTreeSet::from(["fact-1".into()]);
    let admitted = action()
        .admit_finish(&available, ["source-only-unrelated-text"])
        .unwrap();
    assert_eq!(admitted[0].tier, Tier::Agentic);
    assert_eq!(admitted[0].confidence_tier, ConfidenceTier::InferredWeak);
    assert_eq!(admitted[0].finding_id, "finding-1");
    assert_eq!(
        action().admit_finish(&BTreeSet::new(), std::iter::empty()),
        Err(InvestigationError::InvalidCitation)
    );
    // A required ID is a typed reference, not prose replay.
    assert!(action().admit_finish(&available, ["fact-1"]).is_ok());
}

#[test]
fn investigation_replay_checks_uncited_short_split_and_unicode_source() {
    // AC-0184/0188: every supplied string participates, regardless of citations.
    let source = "a_private_implementation_detail_that_must_never_be_persisted_as_model_prose";
    assert_eq!(
        reject_prose_replay([&source[..30], &source[30..]], [source]),
        Err(InvestigationError::SourceReplay)
    );
    assert_eq!(
        reject_prose_replay(["Paraphrased except UNSEEN detail"], ["UNSEEN"]),
        Err(InvestigationError::SourceReplay)
    );
    let unicode = "私".repeat(48);
    let spaced = unicode.chars().map(|c| format!("{c} ")).collect::<String>();
    assert_eq!(
        reject_prose_replay([spaced.as_str()], [unicode.as_str()]),
        Err(InvestigationError::SourceReplay)
    );
    assert_eq!(
        reject_prose_replay(["password=synthetic-secret"], std::iter::empty()),
        Err(InvestigationError::SensitiveOutput)
    );
    assert!(reject_prose_replay(["A new interpretation."], ["entirely unrelated"]).is_ok());
}

#[test]
fn investigation_ledger_rejects_changed_associations_and_unjustified_citations() {
    // AC-0182/0188: a saved manifest is internally exact, never a loose ID list.
    let original = ledger();
    original.validate().unwrap();
    let fingerprint = original.fingerprint().unwrap();
    let mut changed = original.clone();
    changed.citations[0].fact_digest = format!("node-v1:{}", "c".repeat(64));
    assert!(changed.validate().is_err());
    changed = original.clone();
    changed.citations.push(changed.citations[0].clone());
    assert!(changed.validate().is_err());
    changed = original.clone();
    changed.selected_facts[0].binding = Some(crate::TaskSourceAssociation {
        repo_key: "repo-a".into(),
        receipt_id: format!("ts-primary-v2:{}", "d".repeat(64)),
        emitted_fact_digest: changed.selected_facts[0].fact_digest.clone(),
    });
    assert!(changed.validate().is_err());
    changed
        .receipt_references
        .push(InvestigationReceiptReference {
            source_id: "src-one".into(),
            repo_key: "repo-a".into(),
            receipt_id: format!("ts-primary-v2:{}", "d".repeat(64)),
        });
    assert_ne!(changed.fingerprint().unwrap(), fingerprint);
    changed.queries[0].returned_facts = 0;
    assert!(changed.validate().is_err());
}

#[test]
fn investigation_result_roundtrip_retains_basis_and_rejects_authority_tampering() {
    // AC-0187/0188: admission and later reading require no current source lookup.
    let ledger = ledger();
    let result = InvestigationResult::admit(
        "inv-one",
        &ledger,
        &action(),
        ["unrelated-context"],
        Some("fixture-model".into()),
        "2026-09-11T00:00:00Z".into(),
    )
    .unwrap();
    let stored = serde_json::to_string(&result).unwrap();
    let restored: InvestigationResult = decode_record(&stored).unwrap();
    restored.validate(&ledger).unwrap();
    assert_eq!(restored, result);
    let mut changed = restored;
    changed.findings[0].confidence_tier = ConfidenceTier::Confirmed;
    assert!(changed.validate(&ledger).is_err());
    let mut raw = serde_json::to_value(&result).unwrap();
    raw["unexpected"] = json!(true);
    assert!(decode_record::<InvestigationResult>(&raw.to_string()).is_err());
}

#[test]
fn investigation_insufficient_evidence_is_a_valid_honest_finish() {
    // AC-0184: empty unsupported output does not masquerade as scoped findings.
    let mut finish = InvestigationAction::Finish {
        findings: vec![],
        knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
        limitations: vec!["No supporting evidence was found within this bounded scope.".into()],
    };
    assert!(
        finish
            .admit_finish(&BTreeSet::new(), std::iter::empty())
            .is_ok()
    );
    if let InvestigationAction::Finish {
        knowledge_completeness,
        ..
    } = &mut finish
    {
        *knowledge_completeness = KnowledgeCompleteness::Partial;
    }
    assert!(
        finish
            .admit_finish(&BTreeSet::new(), std::iter::empty())
            .is_err()
    );
}

#[test]
fn investigation_record_preflight_rejects_depth_duplicate_and_byte_overflow() {
    // AC-0188: bad shapes are refused before recursive typed body allocation.
    let deep = format!("{}0{}", "[".repeat(34), "]".repeat(34));
    assert!(decode_record::<serde_json::Value>(&deep).is_err());
    assert!(decode_record::<serde_json::Value>("{\"a\":1,\"a\":2}").is_err());
    assert!(
        decode_record::<serde_json::Value>(&format!("\"{}\"", "x".repeat(MAX_RECORD_BYTES)))
            .is_err()
    );
}

#[test]
fn investigation_prepared_input_binds_all_query_text_and_rejects_replacement() {
    // AC-0182/0184: uncited graph strings still participate in admission, and
    // changing the actual query bytes cannot retain the original manifest.
    let private = "Never copy this complete private implementation string into durable findings.";
    let page = json!({"facts":[{"properties":{"implementation":private}}]}).to_string();
    let mut basis = ledger();
    basis.queries[0].response_hash = content_hash(page.as_bytes());
    basis.queries[0].response_bytes = page.len();
    let input = InvestigationInput::prepare(
        "Explain the bounded evidence",
        SpecialistId::DomainAnalyst,
        basis.clone(),
        std::slice::from_ref(&page),
        &[],
        &[],
        &InvestigationUsage::default(),
    )
    .unwrap();
    let payload = input.action("inv-one:step-1".into()).unwrap();
    assert!(payload.payload.prompt.contains(private));
    let mut answer = action();
    if let InvestigationAction::Finish { findings, .. } = &mut answer {
        findings[0].statement = private.into();
    }
    assert_eq!(
        input.admit_result("inv-one", &answer, None, "2026-09-11T00:00:00Z".into()),
        Err(InvestigationError::SourceReplay)
    );
    assert!(
        InvestigationInput::prepare(
            "Explain the bounded evidence",
            SpecialistId::DomainAnalyst,
            basis,
            &[page.replace("private", "replaced")],
            &[],
            &[],
            &InvestigationUsage::default()
        )
        .is_err()
    );
}

#[test]
fn investigation_short_graph_names_remain_usable_but_raw_excerpts_do_not() {
    // AC-0184/0188: per-scalar metadata admission must not prohibit all English
    // prose because a recovered symbol is named a; source excerpts stay private.
    let page = json!({"facts":[{"properties":{"name":"a","value":"false"}}]}).to_string();
    let mut basis = ledger();
    basis.queries[0].response_hash = content_hash(page.as_bytes());
    basis.queries[0].response_bytes = page.len();
    let input = InvestigationInput::prepare(
        "Explain the evidence",
        SpecialistId::DomainAnalyst,
        basis,
        &[page],
        &[],
        &[],
        &InvestigationUsage::default(),
    )
    .unwrap();
    assert!(
        input
            .admit_result("inv-one", &action(), None, "2026-09-11T00:00:00Z".into())
            .is_ok()
    );
    assert_eq!(
        reject_prose_replay(["The source says return false"], ["return false"]),
        Err(InvestigationError::SourceReplay)
    );
}

#[test]
fn investigation_detail_roundtrip_closes_flattened_and_nested_unknown_fields() {
    // AC-0188: reusable public DTOs have a strict new storage contract.
    let detail = InvestigationDetail {
        terminal_diagnostic: None,
        advisory_audit: None,
        completion: None,
        inspection_requirements: None,
        inspection_assessment: None,
        summary: InvestigationSummary {
            completion_outcome: None,
            obligation_status: None,
            inspection_status: None,
            inspection_requirement_count: None,
            schema_version: 1,
            investigation_id: "inv-one".into(),
            conversation_id: "conversation-one".into(),
            parent_id: None,
            job_id: 1,
            specialist_id: SpecialistId::DomainAnalyst,
            question: "Explain checkout".into(),
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            status: InvestigationStatus::Queued,
            revision: 0,
            cancel_requested: false,
            invocation_pending: false,
            actions: InvestigationActions {
                can_cancel: true,
                can_follow_up: false,
            },
            graph_snapshot_id: None,
            last_event_sequence: 1,
            has_result: false,
            created_at: "2026-09-11T00:00:00Z".into(),
            updated_at: "2026-09-11T00:00:00Z".into(),
        },
        context_owner: "context-owner".into(),
        origin: InvestigationOrigin::App,
        specialist: SpecialistId::DomainAnalyst.definition().unwrap(),
        provider: InvestigationProvider {
            mode: InvestigationProviderMode::Local,
            provider_id: "test".into(),
            model: "fixture".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            deployment: None,
            protocol_version: None,
            available: true,
            unavailable_reason: None,
        },
        scope_snapshot_id: None,
        coverage_context: None,
        limits: InvestigationLimits::default(),
        usage: InvestigationUsage::default(),
        citations: vec![],
        error: None,
    };
    let raw = serde_json::to_string(&detail).unwrap();
    assert!(!raw.contains("terminal_diagnostic"));
    assert_eq!(
        serde_json::to_string(&decode_record::<InvestigationDetail>(&raw).unwrap()).unwrap(),
        raw
    );
    assert!(
        serde_json::to_value(&detail)
            .unwrap()
            .get("coverage_context")
            .is_none()
    );
    assert_eq!(decode_record::<InvestigationDetail>(&raw).unwrap(), detail);
    let mut altered = serde_json::to_value(&detail).unwrap();
    altered["unknown"] = json!(true);
    assert!(decode_record::<InvestigationDetail>(&altered.to_string()).is_err());
}

#[test]
fn investigation_nested_prior_finding_prose_cannot_be_replayed() {
    // AC-0184/0188: a short prior statement inside JSON retains source privacy.
    let mut prior_action = action();
    if let InvestigationAction::Finish { findings, .. } = &mut prior_action {
        findings[0].statement = "Reserved internal conclusion".into();
    }
    let parent = InvestigationResult::admit(
        "inv-parent",
        &ledger(),
        &prior_action,
        std::iter::empty(),
        None,
        "2026-09-11T00:00:00Z".into(),
    )
    .unwrap();
    let history = vec![
        json!({
            "parent_investigation_id": "inv-parent",
            "original_graph_snapshot_id": parent.graph_snapshot_id,
            "original_scope": {"type":"all"},
            "saved_execution_status": "completed",
            "cancellation_requested": false,
            "invocation_outcome_pending": false,
            "authority": "Prior findings remain T3/InferredWeak.",
            "saved_result": parent
        })
        .to_string(),
    ];
    let bytes = serde_json::to_vec(&history).unwrap();
    let mut basis = ledger();
    basis.supplied_history_hash = Some(content_hash(&bytes));
    basis.supplied_history_bytes = bytes.len();
    let page = json!({"facts":[]}).to_string();
    basis.queries[0].response_hash = content_hash(page.as_bytes());
    basis.queries[0].response_bytes = page.len();
    let input = InvestigationInput::prepare(
        "Explain evidence",
        SpecialistId::DomainAnalyst,
        basis,
        &[page],
        &[],
        &history,
        &InvestigationUsage::default(),
    )
    .unwrap();
    let mut response = action();
    if let InvestigationAction::Finish { findings, .. } = &mut response {
        findings[0].statement =
            "I agree with the Reserved internal conclusion from earlier.".into();
    }
    assert_eq!(
        input.admit_result("inv-child", &response, None, "2026-09-11T00:00:00Z".into()),
        Err(InvestigationError::SourceReplay)
    );
}

#[test]
fn investigation_output_metadata_cannot_complete_a_split_source_replay() {
    // AC-0184/0188: metadata is joined with all result prose during admission.
    let mut answer = action();
    if let InvestigationAction::Finish { limitations, .. } = &mut answer {
        *limitations = vec!["retained-private-".into()];
    }
    assert_eq!(
        InvestigationResult::admit(
            "inv-one",
            &ledger(),
            &answer,
            ["retained-private-source-value"],
            Some("source-value".into()),
            "2026-09-11T00:00:00Z".into()
        ),
        Err(InvestigationError::SourceReplay)
    );
}

#[path = "disclosure_tests.rs"]
mod disclosure_tests;

#[path = "projection_tests.rs"]
mod projection_tests;

#[path = "sealing_tests.rs"]
pub(super) mod sealing_tests;

#[path = "turn_tests.rs"]
mod turn_tests;

#[path = "inspection_tests.rs"]
pub(super) mod inspection_tests;

#[path = "staged_finish_tests.rs"]
mod staged_finish_tests;

#[test]
fn durable_record_round_trip_bounds_expansion_before_comparison_values() {
    // AC-0188: bound typed expansion before allocating comparison Values.
    use serde::ser::SerializeSeq;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static EMITTED: AtomicUsize = AtomicUsize::new(0);
    #[derive(Debug, serde::Deserialize)]
    struct Expanding {}
    impl serde::Serialize for Expanding {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut sequence = serializer.serialize_seq(Some(MAX_RECORD_BYTES))?;
            for _ in 0..MAX_RECORD_BYTES {
                EMITTED.fetch_add(1, Ordering::Relaxed);
                sequence.serialize_element("expanded")?;
            }
            sequence.end()
        }
    }
    EMITTED.store(0, Ordering::Relaxed);
    assert_eq!(
        decode_record::<Expanding>("{}").unwrap_err(),
        InvestigationError::LimitExceeded
    );
    assert!(EMITTED.load(Ordering::Relaxed) < MAX_RECORD_BYTES);

    // Preserve semantic round-trip comparison for whitespace, escapes, signed
    // numbers and nested objects; canonical byte equality remains a store rule.
    let raw = r#" { "label": "\u00e9", "values": [1, -2, 0.5, null], "nested": {"ok": true} } "#;
    let decoded = decode_record::<serde_json::Value>(raw).unwrap();
    assert_eq!(
        decoded,
        serde_json::from_str::<serde_json::Value>(raw).unwrap()
    );
}

#[test]
fn history_answer_search_markers_keep_legacy_empty_page_bytes() {
    // AC-0402: optional match metadata does not alter unfiltered/legacy page encoding.
    let raw = r#"{"items":[],"next_cursor":null}"#;
    let mut page: InvestigationPage = serde_json::from_str(raw).unwrap();
    assert_eq!(serde_json::to_string(&page).unwrap(), raw);
    // AC-0408: optional name metadata also preserves historical absent-field bytes.
    assert!(page.name_match_ids.is_empty());
    page.name_match_ids.push("pending-task".into());
    assert_eq!(
        serde_json::to_value(&page).unwrap()["name_match_ids"],
        json!(["pending-task"])
    );
    page.answer_match_ids.push("saved-task".into());
    assert_eq!(
        serde_json::to_value(page).unwrap()["answer_match_ids"],
        json!(["saved-task"])
    );
}

#[test]
fn correction_prose_is_checked_even_with_unadmitted_citations() {
    // AC-0565: a recoverable citation error must not hide replay or secrets.
    let private = "Never copy this complete private implementation string into durable findings.";
    let page = json!({"facts":[{"properties":{"implementation":private}}]}).to_string();
    let mut basis = ledger();
    basis.queries[0].response_hash = content_hash(page.as_bytes());
    basis.queries[0].response_bytes = page.len();
    let input = InvestigationInput::prepare(
        "Explain the bounded evidence",
        SpecialistId::DomainAnalyst,
        basis,
        &[page],
        &[],
        &[],
        &InvestigationUsage::default(),
    )
    .unwrap();
    let mut finish = staged_finish::StagedFinish {
        schema_version: 1,
        outcome: staged_finish::CompletionOutcome::Supported,
        narrative: None,
        action: action(),
    };
    if let InvestigationAction::Finish { findings, .. } = &mut finish.action {
        findings[0].citation_ids = vec!["unacquired-target".into()];
    }
    assert_eq!(input.validate_correction_prose(&finish, None), Ok(()));
    assert_eq!(input.correction_replay_location(&finish, None), Ok(None));
    // Passing prose validation does not admit the target as a citation.
    assert_eq!(
        input.admit_result(
            "inv-one",
            &finish.action,
            None,
            "2026-09-11T00:00:00Z".into()
        ),
        Err(InvestigationError::InvalidCitation)
    );
    if let InvestigationAction::Finish { findings, .. } = &mut finish.action {
        findings[0].statement = private.into();
    }
    assert_eq!(
        input.validate_correction_prose(&finish, None),
        Err(InvestigationError::SourceReplay)
    );
    let location = input
        .correction_replay_location(&finish, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        location.start.field,
        ProseField::FindingStatement { finding: 0 }
    );
    assert_eq!(location.end.field, location.start.field);
    assert_eq!(location.start.sentence, 1);
    assert!(!serde_json::to_string(&location).unwrap().contains(private));
    if let InvestigationAction::Finish { findings, .. } = &mut finish.action {
        findings[0].statement = "password=synthetic-secret".into();
    }
    assert_eq!(
        input.validate_correction_prose(&finish, None),
        Err(InvestigationError::SensitiveOutput)
    );
    assert_eq!(
        input.correction_replay_location(&finish, None),
        Err(InvestigationError::SensitiveOutput)
    );
}

#[test]
fn correction_specialist_has_separate_identity_and_retains_acquisition_guards() {
    // AC-0565: @19 evolves the prompt without rewriting saved @18 receipts.
    let old = SpecialistId::DomainAnalystV18;
    let current = SpecialistId::DomainAnalystV19;
    assert_eq!(old.definition().unwrap().version, 18);
    assert_eq!(current.definition().unwrap().version, 19);
    assert_eq!(serde_json::to_value(current).unwrap(), "domain-analyst@19");
    assert_ne!(
        old.definition().unwrap().prompt_fingerprint,
        current.definition().unwrap().prompt_fingerprint
    );
    assert!(current.prompt().starts_with(&old.prompt()));
    assert!(!old.prompt().contains("Unacquired citation correction@1"));
    assert!(current.uses_acquisition_choices());
    assert!(old.uses_acquisition_choices());
    assert!(!SpecialistId::DomainAnalystV17.uses_acquisition_choices());
    assert_eq!(
        old.definition().unwrap().operations,
        current.definition().unwrap().operations
    );
}

// AC-0582: authenticated policy and optional text-free diagnostic regression.
#[test]
fn terminal_diagnostic_wire_is_closed_and_optional() {
    let diagnostic = InvestigationTerminalDiagnostic::Operational {
        phase: InvestigationFailurePhase::SourceReservation,
        error_class: InvestigationOperationalClass::StoreInvalid,
    };
    let value = serde_json::to_value(diagnostic).unwrap();
    assert_eq!(
        value,
        json!({"schema":"investigation-terminal-diagnostic@1", "phase":"source_reservation", "error_class":"store_invalid"})
    );
    assert_eq!(
        serde_json::from_value::<InvestigationTerminalDiagnostic>(value.clone()).unwrap(),
        diagnostic
    );
    for (key, invalid) in [
        ("schema", "unknown@2"),
        ("phase", "provider prose"),
        ("error_class", "sql message"),
        ("response_text", "private canary"),
    ] {
        let mut changed = value.clone();
        changed[key] = json!(invalid);
        assert!(serde_json::from_value::<InvestigationTerminalDiagnostic>(changed).is_err());
    }
}

// AC-0591: all admission variants have an explicit reason, with no prose field.
#[test]
fn rejected_finish_diagnostic_codes_are_closed() {
    for error in [
        InvestigationError::InvalidRequest,
        InvestigationError::InvalidAction,
        InvestigationError::LimitExceeded,
        InvestigationError::InvalidInput,
        InvestigationError::InvalidCitation,
        InvestigationError::SourceReplay,
        InvestigationError::SensitiveOutput,
    ] {
        let diagnostic = InvestigationTerminalDiagnostic::RejectedFinish {
            reason: error.into(),
        };
        let value = serde_json::to_value(diagnostic).unwrap();
        assert_eq!(value["schema"], "investigation-terminal-diagnostic@3");
        assert_eq!(value.as_object().unwrap().len(), 2);
        assert_eq!(
            serde_json::from_value::<InvestigationTerminalDiagnostic>(value.clone()).unwrap(),
            diagnostic
        );
        for (field, text) in [
            ("reason", "provider prose"),
            ("response_text", "forbidden"),
            ("schema", "unknown@4"),
        ] {
            let mut changed = value.clone();
            changed[field] = json!(text);
            assert!(serde_json::from_value::<InvestigationTerminalDiagnostic>(changed).is_err());
        }
    }
}

#[test]
fn auditor14_capacity_terminal_diagnostic_is_closed() {
    let diagnostic = InvestigationTerminalDiagnostic::CapacityCompletion {
        reason: CapacityCompletionReason::UnmetObligationsAfterCapacityExclusion,
    };
    let value = serde_json::to_value(diagnostic).unwrap();
    assert_eq!(
        value,
        json!({"schema":"investigation-terminal-diagnostic@4","reason":"unmet_obligations_after_capacity_exclusion"})
    );
    assert_eq!(
        serde_json::from_value::<InvestigationTerminalDiagnostic>(value.clone()).unwrap(),
        diagnostic
    );
    for (field, text) in [
        ("reason", "source prose"),
        ("response_text", "forbidden"),
        ("schema", "unknown@4"),
    ] {
        let mut changed = value.clone();
        changed[field] = json!(text);
        assert!(serde_json::from_value::<InvestigationTerminalDiagnostic>(changed).is_err());
    }
}

// AC-0725: rejected-action and preparation failure records are closed, text-free wire.
#[test]
fn failure_record_terminal_diagnostics_are_closed() {
    let rejected = InvestigationTerminalDiagnostic::from(ActionDecodeFailure {
        stage: ActionDecodeStage::SemanticRule(ActionSemanticRule::FindingCitations),
        error: InvestigationError::InvalidCitation,
    });
    let preparation = InvestigationTerminalDiagnostic::Preparation {
        phase: InvestigationFailurePhase::Query,
    };
    for (diagnostic, expected) in [
        (
            rejected,
            json!({"schema":"investigation-terminal-diagnostic@5","stage":"finding_citations","reason":"invalid_citation"}),
        ),
        (
            InvestigationTerminalDiagnostic::from(ActionDecodeFailure {
                stage: ActionDecodeStage::Preflight,
                error: InvestigationError::LimitExceeded,
            }),
            json!({"schema":"investigation-terminal-diagnostic@5","stage":"preflight","reason":"limit_exceeded"}),
        ),
        (
            preparation,
            json!({"schema":"investigation-terminal-diagnostic@6","phase":"query"}),
        ),
    ] {
        let value = serde_json::to_value(diagnostic).unwrap();
        assert_eq!(value, expected);
        assert_eq!(
            serde_json::from_value::<InvestigationTerminalDiagnostic>(value.clone()).unwrap(),
            diagnostic
        );
        for field in ["stage", "reason", "phase", "response_text", "schema"] {
            let mut changed = value.clone();
            changed[field] = json!("provider prose");
            assert!(serde_json::from_value::<InvestigationTerminalDiagnostic>(changed).is_err());
        }
    }
}
