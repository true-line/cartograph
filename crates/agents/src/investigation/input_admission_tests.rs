use super::*;
use context_hub::coverage::CoverageQualification;
use obligations::{Obligation, ObligationEnvelope, StagedObligationRequest};
use serde_json::json;

struct Fixture {
    ledger: InvestigationInputLedger,
    request: StagedObligationRequest,
    metadata: ProtectedCompletionMetadata,
    basis: InvestigationTurnBasis,
}
impl Fixture {
    fn new(role: SpecialistId, schema: u32) -> Self {
        let policy = &INVESTIGATION_V2_POLICY;
        let (target, mut ledger, _) = crate::investigation::tests::inspection_tests::fixture();
        ledger.schema_version = 2;
        ledger.coverage = Some(
            CoverageQualification::new(&[], &[])
                .unwrap()
                .durable_proof(&ledger.graph_snapshot_id)
                .unwrap(),
        );
        ledger.revision = 0;
        ledger.selected_facts.clear();
        ledger.receipt_references.clear();
        ledger.citations.clear();
        ledger.queries.clear();
        let mut source = Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(target),
        };
        let id = source.content_id().unwrap();
        if let Obligation::Source { obligation_id, .. } = &mut source {
            *obligation_id = id;
        }
        let request = StagedObligationRequest {
            schema_version: schema,
            request_nonce: "admission-input-fixture".into(),
            specialist_id: serde_json::to_value(role).unwrap().as_str().unwrap().into(),
            obligations: ObligationEnvelope {
                schema_version: 2,
                contract: "investigation-completion-obligations@2".into(),
                obligations: vec![source],
            },
            inspection_requirements: None,
            question: "Inspect retained evidence.".into(),
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            limit_profile: policy.profile().into(),
            expected_graph_revision: Some(ledger.graph_snapshot_id.clone()),
            expected_context_id: ledger.coverage.as_ref().unwrap().context_id.clone(),
            conversation_id: None,
            parent_id: None,
        };
        let metadata = ProtectedCompletionMetadata {
            schema_version: 1,
            request_hash: request.intent_hash_with_policy(policy).unwrap(),
            obligations_hash: request.obligations.fingerprint_with_policy(policy).unwrap(),
            completed_attempt_ids: vec![],
            refusal_count: 0,
            last_refusal_id: None,
            feedback: None,
        };
        let basis = InvestigationTurnBasis {
            input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
            previous_ledger: None,
            completed_tool: None,
        };
        Self {
            ledger,
            request,
            metadata,
            basis,
        }
    }
    fn build(
        &self,
        role: SpecialistId,
        usage: &InvestigationUsage,
        mode: CompletionCallMode,
        count: u32,
        feedback: Option<&AdmissionCapacityFeedback>,
    ) -> Result<InvestigationInput, InvestigationError> {
        InvestigationInput::prepare_v9_with_policy(
            &self.request.question,
            role,
            self.ledger.clone(),
            &[],
            &[],
            &[],
            usage,
            &self.basis,
            &self.request,
            &self.metadata,
            &[],
            mode,
            count,
            feedback,
            &INVESTIGATION_V2_POLICY,
        )
    }
}
fn feedback(reason: CapacityRefusalReason) -> AdmissionCapacityFeedback {
    AdmissionCapacityFeedback {
        schema_version: 1,
        action_hash: "a".repeat(64),
        reason,
    }
}
fn usage(mode: CompletionCallMode) -> InvestigationUsage {
    let limits = INVESTIGATION_V2_POLICY.limits();
    InvestigationUsage {
        model_invocations: if mode == CompletionCallMode::Choice {
            1
        } else {
            limits.model_invocations - 2
        },
        generated_token_reservations: if mode == CompletionCallMode::Choice {
            2048
        } else {
            limits.generated_token_reservations - 4096
        },
        ..Default::default()
    }
}
fn prompt(input: &InvestigationInput) -> Value {
    serde_json::from_str(&input.payload.prompt).unwrap()
}

#[test]
fn admission_prompt_builds_actual_progressive_input_for_both_roles_and_modes() {
    // AC-0287, AC-0439: this builder is internal and grants no dispatch authority.
    for role in [
        SpecialistId::DomainAnalystV10,
        SpecialistId::EvidenceAuditorV10,
        SpecialistId::DomainAnalystV11,
        SpecialistId::DomainAnalystV12,
        SpecialistId::DomainAnalystV13,
        SpecialistId::EvidenceAuditorV11,
    ] {
        let fixture = Fixture::new(role, 7);
        for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
            let input = fixture
                .build(
                    role,
                    &usage(mode),
                    mode,
                    1,
                    Some(&feedback(CapacityRefusalReason::PromptCapacity)),
                )
                .unwrap();
            let body = prompt(&input);
            assert_eq!(input.payload.system, role.prompt());
            assert_eq!(body["schema_version"], 9);
            assert_eq!(
                body["turn_progress"]["contract"],
                "investigation-turn-progress@4"
            );
            assert_eq!(
                body["turn_progress"]["admission_mode"],
                if mode == CompletionCallMode::Choice {
                    "choice"
                } else {
                    "finish_only"
                }
            );
            assert_eq!(
                body["turn_progress"]["phase"],
                if mode == CompletionCallMode::Choice {
                    "initial"
                } else {
                    "finish_only"
                }
            );
            assert_eq!(body["turn_progress"]["last_completed_action"], Value::Null);
            assert_eq!(body["turn_progress"]["capacity_refusal_count"], 1);
            assert_eq!(
                body["completion_metadata"],
                serde_json::to_value(&fixture.metadata).unwrap()
            );
            assert_eq!(
                body["coverage_qualification"],
                serde_json::to_value(&fixture.ledger.coverage.as_ref().unwrap().qualification)
                    .unwrap()
            );
            assert_eq!(
                body["compact_context"]["query_history"]["prior_queries"],
                json!([])
            );
            assert!(!input.payload.prompt.contains("private-plugin-basis"));
        }
    }
}

#[test]
fn admission_prompt_accept_clears_latest_feedback_without_resetting_count() {
    let role = SpecialistId::DomainAnalystV10;
    let fixture = Fixture::new(role, 7);
    let before = fixture
        .build(
            role,
            &usage(CompletionCallMode::Choice),
            CompletionCallMode::Choice,
            1,
            Some(&feedback(CapacityRefusalReason::UnprovenContribution)),
        )
        .unwrap();
    let after = fixture
        .build(
            role,
            &usage(CompletionCallMode::Choice),
            CompletionCallMode::Choice,
            1,
            None,
        )
        .unwrap();
    assert_eq!(
        prompt(&before)["turn_progress"]["capacity_feedback"]["reason"],
        "unproven_contribution"
    );
    assert_eq!(
        prompt(&after)["turn_progress"]["capacity_feedback"],
        Value::Null
    );
    assert_eq!(prompt(&after)["turn_progress"]["capacity_refusal_count"], 1);
}

#[test]
fn admission_prompt_rejects_unbound_mode_count_feedback_and_old_family() {
    let role = SpecialistId::DomainAnalystV10;
    let fixture = Fixture::new(role, 7);
    let actual = usage(CompletionCallMode::Choice);
    assert!(
        fixture
            .build(role, &actual, CompletionCallMode::FinishOnly, 0, None)
            .is_err()
    );
    assert!(
        fixture
            .build(
                role,
                &actual,
                CompletionCallMode::Choice,
                0,
                Some(&feedback(CapacityRefusalReason::PromptCapacity))
            )
            .is_err()
    );
    assert!(
        fixture
            .build(role, &actual, CompletionCallMode::Choice, 2, None)
            .is_err()
    );
    let mut bad = feedback(CapacityRefusalReason::PromptCapacity);
    bad.action_hash = "A".repeat(64);
    assert!(
        fixture
            .build(role, &actual, CompletionCallMode::Choice, 1, Some(&bad))
            .is_err()
    );
    bad = feedback(CapacityRefusalReason::PromptCapacity);
    bad.schema_version = 2;
    assert!(
        fixture
            .build(role, &actual, CompletionCallMode::Choice, 1, Some(&bad))
            .is_err()
    );
    let old = Fixture::new(SpecialistId::DomainAnalystV9, 6);
    assert!(
        old.build(
            SpecialistId::DomainAnalystV9,
            &actual,
            CompletionCallMode::Choice,
            0,
            None
        )
        .is_err()
    );
    assert!(
        InvestigationInput::prepare_v8_with_policy(
            &fixture.request.question,
            role,
            fixture.ledger.clone(),
            &[],
            &[],
            &[],
            &actual,
            &fixture.basis,
            &fixture.request,
            &fixture.metadata,
            &[],
            &INVESTIGATION_V2_POLICY
        )
        .is_err()
    );
}

#[test]
fn admission_prompt_capacity_reason_wire_is_closed_and_size_pinned() {
    // Changing this list/length requires re-proving the sizing envelope.
    for (reason, name, length) in [
        (CapacityRefusalReason::PromptCapacity, "prompt_capacity", 15),
        (
            CapacityRefusalReason::FinishCallReserve,
            "finish_call_reserve",
            19,
        ),
        (
            CapacityRefusalReason::FinishTokenReserve,
            "finish_token_reserve",
            20,
        ),
        (
            CapacityRefusalReason::UnprovenContribution,
            "unproven_contribution",
            21,
        ),
    ] {
        assert_eq!(
            serde_json::to_string(&reason).unwrap(),
            format!("\"{name}\"")
        );
        assert_eq!(reason.encoded_name(), name);
        assert_eq!(name.len(), length);
        assert_eq!(
            serde_json::from_value::<CapacityRefusalReason>(json!(name)).unwrap(),
            reason
        );
    }
    assert!(serde_json::from_value::<CapacityRefusalReason>(json!("storage_capacity")).is_err());
    let mut raw = serde_json::to_value(feedback(CapacityRefusalReason::PromptCapacity)).unwrap();
    raw["source_text"] = "not permitted".into();
    assert!(serde_json::from_value::<AdmissionCapacityFeedback>(raw).is_err());
}

fn assert_dominates(
    bound: llm::bounded::BoundedWireMeasurements,
    actual: llm::bounded::BoundedWireMeasurements,
) {
    assert!(bound.raw_string_bytes >= actual.raw_string_bytes);
    assert!(bound.original_payload_bytes >= actual.original_payload_bytes);
    assert!(bound.redacted_payload_bytes >= actual.redacted_payload_bytes);
    assert!(bound.original_action_bytes >= actual.original_action_bytes);
    assert!(bound.egress_fingerprint_bytes >= actual.egress_fingerprint_bytes);
}

#[test]
fn admission_prompt_reserve_dominates_actual_simultaneous_feedback_in_both_modes() {
    // AC-0287: real builders and the real redactor/tuple encoder; no dispatch.
    use llm::{LlmProvider, OllamaProvider};
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let profile = provider.bounded_profile().unwrap();
    let limits = llm::bounded::CompletionLimits::default();
    for role in [
        SpecialistId::DomainAnalystV10,
        SpecialistId::EvidenceAuditorV10,
        SpecialistId::DomainAnalystV13,
    ] {
        let mut fixture = Fixture::new(role, 7);
        // Immutable source-authored strings stay fixed even when redaction
        // expands them; only the finite metadata/feedback slots are reserved.
        fixture.request.question = "Inspect sk-abcdefgh and Unicode é.".into();
        fixture.metadata.request_hash = fixture
            .request
            .intent_hash_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        let initial = fixture
            .build(
                role,
                &usage(CompletionCallMode::Choice),
                CompletionCallMode::Choice,
                0,
                None,
            )
            .unwrap();
        let reserved = initial
            .measure_v9_reserve_with_policy(
                &profile,
                &limits,
                "inv:admission-reserve",
                &INVESTIGATION_V2_POLICY,
            )
            .unwrap();
        fixture.metadata.completed_attempt_ids = (0..INVESTIGATION_V2_POLICY
            .max_completed_attempts())
            .map(|index| format!("{index:064x}"))
            .collect();
        fixture.metadata.refusal_count = 1;
        fixture.metadata.last_refusal_id = Some("e".repeat(64));
        fixture.metadata.feedback = Some(staged_finish::RefusalFeedback {
            citation_drop: None,
            prose_correction: None,
            schema_version: 1,
            citation_correction: None,
            unmet: fixture
                .request
                .obligations
                .obligations
                .iter()
                .map(|item| {
                    let (kind, id) = item.key();
                    staged_finish::FeedbackEntry {
                        obligation_id: id.into(),
                        kind: kind.into(),
                        reason: staged_finish::RefusalReason::AcquiredNotSupplied,
                    }
                })
                .collect(),
        });
        for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
            let mut consumed = usage(mode);
            if mode == CompletionCallMode::FinishOnly {
                consumed.model_invocations = INVESTIGATION_V2_POLICY.limits().model_invocations - 1;
                consumed.generated_token_reservations = INVESTIGATION_V2_POLICY
                    .limits()
                    .generated_token_reservations
                    - 2048;
            }
            consumed.reported_input_tokens = Some(u64::MAX);
            consumed.reported_output_tokens = Some(u64::MAX);
            // AC-0418: reserve a present cache key even when the baseline omits it.
            consumed.reported_cache_read_tokens = Some(u64::MAX);
            for reason in [
                CapacityRefusalReason::PromptCapacity,
                CapacityRefusalReason::FinishCallReserve,
                CapacityRefusalReason::FinishTokenReserve,
                CapacityRefusalReason::UnprovenContribution,
            ] {
                let input = fixture
                    .build(
                        role,
                        &consumed,
                        mode,
                        consumed.model_invocations,
                        Some(&feedback(reason)),
                    )
                    .unwrap();
                let body = prompt(&input);
                assert!(body["completion_metadata"]["feedback"].is_object());
                assert!(body["turn_progress"]["capacity_feedback"].is_object());
                let actual = llm::bounded::measure_bounded_action(
                    &profile,
                    &input.action("inv:admission-reserve".into()).unwrap(),
                    &limits,
                )
                .unwrap();
                assert_dominates(
                    if mode == CompletionCallMode::Choice {
                        reserved.choice
                    } else {
                        reserved.finish_only
                    },
                    actual,
                );
            }
        }
    }
}

#[test]
fn admission_prompt_reserve_rejects_legacy_and_mismatched_contract() {
    use llm::{LlmProvider, OllamaProvider};
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let limits = llm::bounded::CompletionLimits::default();
    let role = SpecialistId::DomainAnalystV10;
    let fixture = Fixture::new(role, 7);
    let mut input = fixture
        .build(
            role,
            &usage(CompletionCallMode::Choice),
            CompletionCallMode::Choice,
            0,
            None,
        )
        .unwrap();
    let mut body = prompt(&input);
    body["completion_metadata"]["request_hash"] = "f".repeat(64).into();
    input.payload.prompt = body.to_string();
    assert!(
        input
            .measure_v9_reserve_with_policy(
                &provider.bounded_profile().unwrap(),
                &limits,
                "inv:reserve",
                &INVESTIGATION_V2_POLICY
            )
            .is_err()
    );
    let old = Fixture::new(SpecialistId::DomainAnalystV9, 6);
    let input = InvestigationInput::prepare_v8_with_policy(
        &old.request.question,
        SpecialistId::DomainAnalystV9,
        old.ledger,
        &[],
        &[],
        &[],
        &usage(CompletionCallMode::Choice),
        &old.basis,
        &old.request,
        &old.metadata,
        &[],
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    assert!(
        input
            .measure_v9_reserve_with_policy(
                &provider.bounded_profile().unwrap(),
                &limits,
                "inv:reserve",
                &INVESTIGATION_V2_POLICY
            )
            .is_err()
    );
    let body = prompt(&input);
    assert_eq!(body["schema_version"], 8);
    assert!(body["turn_progress"].get("capacity_feedback").is_none());
    assert!(body["turn_progress"].get("admission_mode").is_none());
}

#[test]
fn admission_prompt_borrowed_pages_preserve_exact_query_projection_and_progress() {
    let role = SpecialistId::DomainAnalystV10;
    let mut fixture = Fixture::new(role, 7);
    let previous = fixture.ledger.clone();
    let raw = json!({"context":{"context_id":fixture.request.expected_context_id,
        "coverage":fixture.ledger.coverage.as_ref().unwrap().qualification,
        "response":{"snapshot_id":fixture.ledger.scope_snapshot_id,"view":"recovered_graph",
        "total_selected":0,"facts":[],"next_cursor":null}},"evidence_options":[]})
    .to_string();
    fixture.ledger.revision = 1;
    fixture.ledger.queries.push(InvestigationQueryManifest {
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
        returned_facts: 0,
        total_selected: 0,
        has_more: false,
    });
    fixture.basis = InvestigationTurnBasis {
        input_ledger_hash: fixture
            .ledger
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap(),
        previous_ledger: Some(previous),
        completed_tool: Some(InvestigationTool::QueryContext),
    };
    let owned = ProgressiveInvestigationContext::prepare(
        &fixture.metadata.request_hash,
        &fixture.ledger,
        std::slice::from_ref(&raw),
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    let borrowed = ProgressiveInvestigationContext::prepare_raw(
        &fixture.metadata.request_hash,
        &fixture.ledger,
        &[raw.as_str()],
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_vec(&owned).unwrap(),
        serde_json::to_vec(&borrowed).unwrap()
    );
    for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
        let input = InvestigationInput::prepare_v9_with_policy(
            &fixture.request.question,
            role,
            fixture.ledger.clone(),
            &[raw.as_str()],
            &[],
            &[],
            &usage(mode),
            &fixture.basis,
            &fixture.request,
            &fixture.metadata,
            &[],
            mode,
            0,
            None,
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
        let body = prompt(&input);
        assert_eq!(
            body["compact_context"],
            serde_json::to_value(&borrowed).unwrap()
        );
        assert_eq!(
            body["turn_progress"]["last_completed_action"]["type"],
            "query_context"
        );
        assert!(body["turn_progress"]["last_completed_action"]["query_id"].is_string());
        assert!(
            body["turn_progress"]["last_completed_action"]
                .get("query")
                .is_none()
        );
        assert_eq!(
            body["turn_progress"]["phase"],
            if mode == CompletionCallMode::Choice {
                "after_query"
            } else {
                "finish_only"
            }
        );
    }
}

fn source_fixture(
    role: SpecialistId,
    text: &str,
) -> (
    Fixture,
    String,
    InvestigationInspectionTarget,
    InvestigationCitation,
) {
    let policy = &INVESTIGATION_V2_POLICY;
    let mut fixture = Fixture::new(role, 7);
    let (mut target, mut ledger, _) = crate::investigation::tests::inspection_tests::fixture();
    target.source.byte_end = target.source.byte_start + text.len() as u64;
    target.captured.byte_end = target.source.byte_end;
    target.captured.file.byte_len = target.source.byte_end;
    target.captured.file.digest = content_hash(text.as_bytes());
    target.target_id = target.content_id().unwrap();
    let mut source = Obligation::Source {
        obligation_id: String::new(),
        target: Box::new(target.clone()),
    };
    let id = source.content_id().unwrap();
    if let Obligation::Source { obligation_id, .. } = &mut source {
        *obligation_id = id;
    }
    fixture.request.obligations.obligations = vec![source];
    fixture.metadata.request_hash = fixture.request.intent_hash_with_policy(policy).unwrap();
    fixture.metadata.obligations_hash = fixture
        .request
        .obligations
        .fingerprint_with_policy(policy)
        .unwrap();
    let prototype = ledger.citations[1].clone();
    ledger.schema_version = 2;
    ledger.coverage = fixture.ledger.coverage.clone();
    ledger.revision = 1;
    ledger.citations.truncate(1);
    ledger.queries.clear();
    let raw=json!({"context":{"context_id":fixture.request.expected_context_id,
        "coverage":ledger.coverage.as_ref().unwrap().qualification,
        "response":{"snapshot_id":ledger.scope_snapshot_id,"view":"recovered_graph","total_selected":1,
            "facts":[{"reference":target.fact,"label":"BusinessRule"}],"next_cursor":null}},
        "evidence_options":[{"fact":target.fact,"origin":"captured_primary_source_metadata","ranges":[]}]}).to_string();
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
    fixture.basis = InvestigationTurnBasis {
        input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
        previous_ledger: Some(fixture.ledger),
        completed_tool: Some(InvestigationTool::QueryContext),
    };
    fixture.ledger = ledger;
    (fixture, raw, target, prototype)
}
fn build_source(
    fixture: &Fixture,
    raw: &str,
    evidence: &[AgentEvidence],
    role: SpecialistId,
    mode: CompletionCallMode,
) -> InvestigationInput {
    InvestigationInput::prepare_v9_with_policy(
        &fixture.request.question,
        role,
        fixture.ledger.clone(),
        &[raw],
        evidence,
        &[],
        &usage(mode),
        &fixture.basis,
        &fixture.request,
        &fixture.metadata,
        &[],
        mode,
        0,
        None,
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap()
}
fn acquire_source(
    fixture: &mut Fixture,
    target: &InvestigationInspectionTarget,
    prototype: &InvestigationCitation,
    evidence: &mut Vec<AgentEvidence>,
    text: &str,
) {
    let previous = fixture.ledger.clone();
    let id = format!("evidence-{}", evidence.len() + 1);
    let mut citation = prototype.clone();
    citation.citation_id = id.clone();
    citation.source = Some(target.source.clone());
    citation.text_hash = Some(content_hash(text.as_bytes()));
    citation.origin = InvestigationEvidenceOrigin::CapturedPrimarySource {
        registered_source_id: target.registered_source_id.clone(),
        receipt_id: target.receipt_id.clone(),
        receipt_inventory_index: target.receipt_inventory_index,
        captured: target.captured.clone(),
        scope: PrimarySourceScope::PrimarySourceOnly,
        input_closure: InputClosureStatus::InputClosureNotEstablished,
    };
    fixture.ledger.citations.push(citation);
    fixture.ledger.revision += 1;
    fixture.basis = InvestigationTurnBasis {
        input_ledger_hash: fixture
            .ledger
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap(),
        previous_ledger: Some(previous),
        completed_tool: Some(InvestigationTool::ReadEvidence),
    };
    evidence.push(AgentEvidence {
        id,
        source: target.source.clone(),
        text: text.into(),
    });
}

#[test]
fn admission_unread_source_bound_dominates_actual_first_and_repeated_read_inputs() {
    // AC-0287: production input construction and wire/redaction; no source I/O.
    use llm::{LlmProvider, OllamaProvider};
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let profile = provider.bounded_profile().unwrap();
    let limits = llm::bounded::CompletionLimits::default();
    for text in [
        "private text".into(),
        "é中🙂".into(),
        "ghp_abcdefgh".into(),
        "\"\\\n\t\u{0}".into(),
        "secret=Bearer abcdefgh".into(),
        "\u{1}".repeat(8192),
    ] {
        for role in [
            SpecialistId::DomainAnalystV10,
            SpecialistId::EvidenceAuditorV10,
        ] {
            let (mut fixture, raw, target, prototype) = source_fixture(role, &text);
            let mut evidence = vec![];
            let mut current =
                build_source(&fixture, &raw, &evidence, role, CompletionCallMode::Choice);
            for repeated in [false, true] {
                let bound = current
                    .measure_unread_source_v9_with_policy(
                        &target,
                        &profile,
                        &limits,
                        "inv:unread-source",
                        &INVESTIGATION_V2_POLICY,
                    )
                    .unwrap();
                acquire_source(&mut fixture, &target, &prototype, &mut evidence, &text);
                for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
                    let actual = build_source(&fixture, &raw, &evidence, role, mode);
                    let body = prompt(&actual);
                    assert_eq!(
                        body["turn_progress"]["last_completed_action"]["previously_acquired"],
                        repeated
                    );
                    let measured = llm::bounded::measure_bounded_action(
                        &profile,
                        &actual.action("inv:unread-source".into()).unwrap(),
                        &limits,
                    )
                    .unwrap();
                    assert_dominates(
                        if mode == CompletionCallMode::Choice {
                            bound.choice
                        } else {
                            bound.finish_only
                        },
                        measured,
                    );
                    assert_eq!(
                        body["inspection_progress"]["requirements"][0]["acquired"],
                        true
                    );
                    if mode == CompletionCallMode::Choice {
                        current = actual;
                    }
                }
            }
        }
    }
}

#[test]
fn admission_unread_source_refuses_rebound_or_unproven_descriptors_and_shapes() {
    use llm::{LlmProvider, OllamaProvider};
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let profile = provider.bounded_profile().unwrap();
    let limits = llm::bounded::CompletionLimits::default();
    let role = SpecialistId::DomainAnalystV10;
    let (fixture, raw, target, _) = source_fixture(role, "private text");
    let input = build_source(&fixture, &raw, &[], role, CompletionCallMode::Choice);
    for field in ["receipt", "context", "digest", "schema"] {
        let mut changed = target.clone();
        match field {
            "receipt" => changed.receipt_id = format!("ts-primary-v2:{}", "f".repeat(64)),
            "context" => changed.graph_snapshot_id = format!("context-v1:{}", "f".repeat(64)),
            "digest" => changed.captured.file.digest = "f".repeat(64),
            "schema" => changed.schema_version = 2,
            _ => unreachable!(),
        }
        changed.target_id = changed.content_id().unwrap();
        assert!(
            input
                .measure_unread_source_v9_with_policy(
                    &changed,
                    &profile,
                    &limits,
                    "inv:unread-source",
                    &INVESTIGATION_V2_POLICY
                )
                .is_err(),
            "{field}"
        );
    }
    for field in [
        "unknown_progress",
        "unknown_usage",
        "omitted",
        "citations",
        "registry",
    ] {
        let mut changed = input.clone();
        let mut body = prompt(&changed);
        match field {
            "unknown_progress" => body["turn_progress"]["future"] = Value::Null,
            "unknown_usage" => body["consumed_budgets"]["future"] = Value::Null,
            "omitted" => body["omitted_sources"] = json!([]),
            "citations" => body["citations"] = json!([]),
            "registry" => {
                body["compact_context"]["query_history"]["registry_hash"] =
                    "new-prefix:unexpected".into()
            }
            _ => unreachable!(),
        }
        changed.payload.prompt = body.to_string();
        assert!(
            changed
                .measure_unread_source_v9_with_policy(
                    &target,
                    &profile,
                    &limits,
                    "inv:unread-source",
                    &INVESTIGATION_V2_POLICY
                )
                .is_err(),
            "{field}"
        );
    }
}

#[test]
fn admission_reserve_redaction_boundary_witnesses_preserve_the_wire_bound() {
    // AC-0287: redact-text@2 quote barriers and private-key cross-field spans.
    // AC-0579 / #1753: an authored string that ends in a compound credential name, with
    // or without a separator, must not reach into the next protocol field.
    // These are byte-capacity witnesses, not proof protected fields still seal.
    use llm::{LlmProvider, OllamaProvider};
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let profile = provider.bounded_profile().unwrap();
    let limits = llm::bounded::CompletionLimits::default();
    let role = SpecialistId::DomainAnalystV10;
    for (question, cross_fields) in [
        ("secret=", false),
        ("password:", false),
        ("DB_PASSWORD=", false),
        ("client_secret", false),
        ("secret_key =>", false),
        ("Bearer ", false),
        ("sk-", false),
        ("AKIA", false),
        ("-----END PRIVATE KEY-----", true),
    ] {
        let mut fixture = Fixture::new(role, 7);
        fixture.request.question = question.into();
        if cross_fields {
            let Obligation::Source {
                obligation_id,
                target,
            } = &mut fixture.request.obligations.obligations[0]
            else {
                unreachable!()
            };
            target.source.path = "-----BEGIN PRIVATE KEY-----".into();
            target.captured.file.path = target.source.path.clone();
            target.target_id = target.content_id().unwrap();
            *obligation_id = Obligation::Source {
                obligation_id: String::new(),
                target: target.clone(),
            }
            .content_id()
            .unwrap();
        }
        fixture.metadata.request_hash = fixture
            .request
            .intent_hash_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        fixture.metadata.obligations_hash = fixture
            .request
            .obligations
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        let initial = fixture
            .build(
                role,
                &usage(CompletionCallMode::Choice),
                CompletionCallMode::Choice,
                0,
                None,
            )
            .unwrap();
        let bound = initial
            .measure_v9_reserve_with_policy(
                &profile,
                &limits,
                "inv:boundary",
                &INVESTIGATION_V2_POLICY,
            )
            .unwrap();
        fixture.metadata.refusal_count = 1;
        fixture.metadata.last_refusal_id = Some("d".repeat(64));
        fixture.metadata.completed_attempt_ids = (0..INVESTIGATION_V2_POLICY
            .max_completed_attempts())
            .map(|index| format!("{index:064x}"))
            .collect();
        for completion_reason in [
            staged_finish::RefusalReason::NotAttempted,
            staged_finish::RefusalReason::AcquiredNotSupplied,
            staged_finish::RefusalReason::Redacted,
            staged_finish::RefusalReason::HashMismatch,
            staged_finish::RefusalReason::Unavailable,
        ] {
            let item = &fixture.request.obligations.obligations[0];
            let (kind, id) = item.key();
            fixture.metadata.feedback = Some(staged_finish::RefusalFeedback {
                citation_drop: None,
                prose_correction: None,
                schema_version: 1,
                citation_correction: None,
                unmet: vec![staged_finish::FeedbackEntry {
                    obligation_id: id.into(),
                    kind: kind.into(),
                    reason: completion_reason,
                }],
            });
            for capacity_reason in [
                CapacityRefusalReason::PromptCapacity,
                CapacityRefusalReason::FinishCallReserve,
                CapacityRefusalReason::FinishTokenReserve,
                CapacityRefusalReason::UnprovenContribution,
            ] {
                for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
                    let mut consumed = usage(mode);
                    if mode == CompletionCallMode::FinishOnly {
                        consumed.model_invocations += 1;
                        consumed.generated_token_reservations += 2048;
                    }
                    let input = fixture
                        .build(role, &consumed, mode, 1, Some(&feedback(capacity_reason)))
                        .unwrap();
                    let actual = llm::bounded::measure_bounded_action(
                        &profile,
                        &input.action("inv:boundary".into()).unwrap(),
                        &limits,
                    )
                    .unwrap();
                    if cross_fields {
                        assert!(actual.redacted_payload_bytes < actual.original_payload_bytes);
                    }
                    assert_dominates(
                        if mode == CompletionCallMode::Choice {
                            bound.choice
                        } else {
                            bound.finish_only
                        },
                        actual,
                    );
                }
            }
        }
    }
}

#[path = "admission_sealing_tests.rs"]
mod sealing_tests;

#[path = "input_source_preview_tests.rs"]
mod source_preview_tests;

#[test]
fn analyst_schema_is_exact_for_role_mode_and_reserve() {
    // AC-0445: real preparation, firewall and sealing; no provider execution.
    use llm::{EgressFirewall, EgressPolicy, LlmProvider, OllamaProvider};
    let role = SpecialistId::DomainAnalystV13;
    let fixture = Fixture::new(role, 7);
    let provider = OllamaProvider::local_default().unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let limits = llm::bounded::CompletionLimits::default();
    for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
        let input = fixture.build(role, &usage(mode), mode, 0, None).unwrap();
        let expected = response_schema::select(role, mode).unwrap();
        assert_eq!(input.payload.response_schema.as_deref(), Some(expected));
        let action = input.action("inv:schema".into()).unwrap();
        let prepared = firewall
            .prepare_bounded(&provider, &action, &limits)
            .unwrap();
        let openai = llm::openai_compatible::OpenAiCompatibleProvider::new(
            "http://127.0.0.1:8000",
            "test",
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        let openai_prepared = firewall.prepare_bounded(&openai, &action, &limits).unwrap();
        assert_eq!(
            openai_prepared.preview().egress.payload.response_schema,
            action.payload.response_schema
        );
        // #1709: Anthropic now compiles the projected schema; the preview still
        // carries the exact original schema for host-side validation.
        let anthropic =
            llm::anthropic::AnthropicProvider::new(llm::anthropic::ClaudeLane::Haiku, "test")
                .unwrap();
        let preview_firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([action.tier]));
        assert_eq!(
            preview_firewall
                .prepare_bounded(&anthropic, &action, &limits)
                .unwrap()
                .preview()
                .egress
                .payload
                .response_schema,
            action.payload.response_schema
        );
        assert_eq!(
            prepared.preview().egress.payload.response_schema,
            action.payload.response_schema
        );
        assert!(
            input
                .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
                .unwrap()
                .is_some()
        );
        for replacement in [
            None,
            Some("{}".into()),
            response_schema::select(
                role,
                if mode == CompletionCallMode::Choice {
                    CompletionCallMode::FinishOnly
                } else {
                    CompletionCallMode::Choice
                },
            )
            .map(str::to_owned),
        ] {
            let mut changed = input.clone();
            changed.payload.response_schema = replacement;
            assert!(
                changed
                    .admission_reserve_basis(&INVESTIGATION_V2_POLICY)
                    .is_err()
            );
            let prepared = firewall
                .prepare_bounded(
                    &provider,
                    &changed.action("inv:schema".into()).unwrap(),
                    &limits,
                )
                .unwrap();
            assert!(
                changed
                    .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
                    .is_err()
            );
        }
        assert!(
            input
                .measure_v9_reserve_with_policy(
                    &provider.bounded_profile().unwrap(),
                    &limits,
                    "inv:schema",
                    &INVESTIGATION_V2_POLICY
                )
                .is_ok()
        );
    }
    let old = Fixture::new(SpecialistId::DomainAnalystV12, 7)
        .build(
            SpecialistId::DomainAnalystV12,
            &usage(CompletionCallMode::Choice),
            CompletionCallMode::Choice,
            0,
            None,
        )
        .unwrap();
    assert!(old.payload.response_schema.is_none());
    assert_eq!(
        SpecialistId::DomainAnalystV12
            .definition()
            .unwrap()
            .prompt_fingerprint,
        "4cc0655d2e7954c4062c6d7c33cfef2f2d87ccc4a721398104a7afd7c675a4c8"
    );
    assert_ne!(
        role.definition().unwrap().prompt_fingerprint,
        SpecialistId::DomainAnalystV12
            .definition()
            .unwrap()
            .prompt_fingerprint
    );
}

#[path = "admitted_schema_tests.rs"]
mod admitted_schema_tests;

// AC-0480: pure typed choices, not dispatch or completed-read authentication.
#[test]
fn gated_analyst_input_schema_and_membership_share_authority() {
    let role = SpecialistId::DomainAnalystV18;
    let build = |id: &str, mode| {
        let mut f = Fixture::new(role, 7);
        if let Obligation::Source {
            target,
            obligation_id,
        } = &mut f.request.obligations.obligations[0]
        {
            target.fact = crate::TaskFactKey::Node { id: id.into() };
            target.target_id = target.content_id().unwrap();
            *obligation_id = Obligation::Source {
                target: target.clone(),
                obligation_id: String::new(),
            }
            .content_id()
            .unwrap();
        }
        f.metadata.request_hash = f
            .request
            .intent_hash_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        f.metadata.obligations_hash = f
            .request
            .obligations
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: f.metadata.request_hash.clone(),
            obligations_hash: f.metadata.obligations_hash.clone(),
            obligations: f.request.obligations.clone(),
        };
        let choices = std::sync::Arc::new(
            acquisition_choices::AcquisitionChoices::derive(
                &contract,
                &f.metadata,
                "task",
                &f.ledger,
                &[],
                &INVESTIGATION_V2_POLICY,
            )
            .unwrap(),
        );
        let input = f.build(role, &usage(mode), mode, 0, None).unwrap();
        (input, choices)
    };
    let (base, choices) = build("node:chosen", CompletionCallMode::Choice);
    assert!(base.action("step".into()).is_err());
    let mut input = base.with_acquisition_choices(choices.clone()).unwrap();
    input.assert_retained_workspace(&INVESTIGATION_V2_POLICY);
    let action = input.action("step".into()).unwrap();
    let prompt: Value = serde_json::from_str(&action.payload.prompt).unwrap();
    let schema: Value =
        serde_json::from_str(action.payload.response_schema.as_ref().unwrap()).unwrap();
    assert_eq!(
        prompt["turn_progress"]["acquisition_choices"],
        serde_json::to_value(&*choices).unwrap()
    );
    assert!(prompt["turn_progress"]["action_feedback"].is_null());
    assert_eq!(
        schema["anyOf"][2]["enum"][0],
        prompt["turn_progress"]["acquisition_choices"]["entries"][0]["action"]
    );
    let context = input.ledger().coverage.as_ref().unwrap().context_id.clone();
    let read =
        |choices: &acquisition_choices::AcquisitionChoices| match &choices.entries()[0].action {
            acquisition_choices::AcquisitionAction::QueryContext { query, .. } => {
                QualifiedInvestigationAction::QueryContext {
                    context_id: context.clone(),
                    query: query.clone(),
                }
            }
            _ => panic!("initial exact query"),
        };
    assert!(input.permits_acquisition(&read(&choices)));
    let (foreign, other) = build("node:other", CompletionCallMode::Choice);
    assert!(!input.permits_acquisition(&read(&other)));
    assert!(foreign.with_acquisition_choices(choices.clone()).is_err());
    let (finish, finish_choices) = build("node:chosen", CompletionCallMode::FinishOnly);
    let finish = finish
        .with_acquisition_admission(
            finish_choices,
            Some(ActionRefusalFeedback {
                query_budget: None,
                reason: ActionRefusalReason::ActionNotOffered,
                patch_rejection: None,
                invocation_ordinal: 1,
                action_hash: "a".repeat(64),
                choices_hash: choices.stamp_hash().unwrap(),
            }),
        )
        .unwrap();
    finish.assert_retained_workspace(&INVESTIGATION_V2_POLICY);
    assert_eq!(
        finish
            .action("step".into())
            .unwrap()
            .payload
            .response_schema
            .as_deref(),
        Some(response_schema::FINISH)
    );
    assert!(!finish.permits_acquisition(&read(&choices)));
    let mut stale = input.clone();
    stale.ledger.revision += 1;
    assert!(stale.action("step".into()).is_err());
    assert!(!stale.permits_acquisition(&read(&choices)));
    let provider = llm::OllamaProvider::local_default().unwrap();
    let prepared = llm::EgressFirewall::new(llm::EgressPolicy::default())
        .prepare_bounded(
            &provider,
            &action,
            &llm::bounded::CompletionLimits::default(),
        )
        .unwrap();
    assert!(prepared.matches_action(&action));
    input
        .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
        .unwrap()
        .unwrap();
    // AC-0484: use the exact actual sealer authority, not a disclosure-only
    // shape check. The positive seal above precedes every stamp mutation.
    let admission = input
        .disclosed_admission_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    for mutation in ["missing", "choices", "feedback"] {
        let mut forged = admission.clone();
        match mutation {
            "missing" => forged.acquisition = None,
            "choices" => forged.acquisition.as_mut().unwrap().choices_hash = "f".repeat(64),
            "feedback" => {
                forged.acquisition.as_mut().unwrap().action_feedback_hash = Some("f".repeat(64))
            }
            _ => unreachable!(),
        }
        assert!(
            sealing::seal_admission_inner(
                input.ledger(),
                &action.payload,
                &prepared,
                &INVESTIGATION_V2_POLICY,
                input.inspection_requirements.as_deref(),
                Some(forged),
                Some(&input)
            )
            .is_err(),
            "{mutation}"
        );
    }
    for (old, pin) in [
        (
            SpecialistId::DomainAnalystV15,
            "ec99aee7d993cae30220094c8afb31430ba33a95b77cd4389ae81fbe0ef0041c",
        ),
        (
            SpecialistId::DomainAnalystV17,
            "3bbbb030e6c75e5d6b7adf9a00ff26597da61a772f4f77c384e11fdc348902a2",
        ),
    ] {
        assert_eq!(old.definition().unwrap().prompt_fingerprint, pin);
        let historical = Fixture::new(old, 7)
            .build(
                old,
                &usage(CompletionCallMode::Choice),
                CompletionCallMode::Choice,
                0,
                None,
            )
            .unwrap();
        if old == SpecialistId::DomainAnalystV17 {
            let original =
                std::mem::replace(&mut input.payload.system, historical.payload.system.clone());
            assert!(
                input
                    .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
                    .is_err()
            );
            input.payload.system = original;
        }
        assert!(
            historical
                .with_acquisition_choices(choices.clone())
                .is_err()
        );
    }
}

// AC-0582: protected schemas/choices use the exact versioned policy, not V2 defaults.
#[test]
fn large_history_admission_schema_and_choices_preserve_effective_policy() {
    let policy = INVESTIGATION_V2_POLICY
        .with_hosted_generation()
        .unwrap()
        .with_parent_history_32768()
        .unwrap();
    let raw = json!({"parent_investigation_id":"parent","original_graph_snapshot_id":null,
        "original_scope":{"type":"all"},"saved_execution_status":"completed",
        "cancellation_requested":false,"invocation_outcome_pending":false,
        "authority":"Prior findings remain T3/InferredWeak. ".repeat(50),"saved_result":null})
    .to_string();
    let history = vec![raw; 10];
    let bytes = serde_json::to_vec(&history).unwrap();
    assert!(bytes.len() > 16384 && bytes.len() <= 32768);
    for role in [
        SpecialistId::DomainAnalystV13,
        SpecialistId::DomainAnalystV18,
        SpecialistId::DomainAnalystV27,
    ] {
        let mut f = Fixture::new(role, 7);
        f.ledger.supplied_history_hash = Some(core_prov::content_hash(&bytes));
        f.ledger.supplied_history_bytes = bytes.len();
        f.basis.input_ledger_hash = f.ledger.fingerprint_with_policy(&policy).unwrap();
        let input = InvestigationInput::prepare_v9_with_policy(
            &f.request.question,
            role,
            f.ledger.clone(),
            &[],
            &[],
            &history,
            &usage(CompletionCallMode::Choice),
            &f.basis,
            &f.request,
            &f.metadata,
            &[],
            CompletionCallMode::Choice,
            0,
            None,
            &policy,
        )
        .unwrap();
        let input = if role.uses_acquisition_choices() {
            let contract = InvestigationCompletionContract {
                schema_version: 1,
                request_hash: f.metadata.request_hash.clone(),
                obligations_hash: f.metadata.obligations_hash.clone(),
                obligations: f.request.obligations.clone(),
            };
            let choices = acquisition_choices::AcquisitionChoices::derive(
                &contract,
                &f.metadata,
                "task",
                &f.ledger,
                &[],
                &policy,
            )
            .unwrap();
            input
                .with_acquisition_choices(std::sync::Arc::new(choices))
                .unwrap()
        } else {
            input
        };
        assert!(input.admission_reserve_basis(&policy).is_ok());
        assert!(
            input
                .admission_reserve_basis(&INVESTIGATION_V2_POLICY)
                .is_err()
        );
        assert!(input.action("step".into()).is_ok());
        let prompt = prompt(&input);
        assert_eq!(
            prompt["limits"],
            serde_json::to_value(policy.limits()).unwrap()
        );
    }
}

// AC-0585: only the exact host-offered obligation node at MAX_QUERY_BYTES
// is irreducible; the worker must terminate its budget failure immediately.
#[test]
fn auditor12_irreducible_page_requires_exact_maximum_offered_obligation() {
    let role = SpecialistId::EvidenceAuditorV12;
    let f = Fixture::new(role, 7);
    let contract = InvestigationCompletionContract {
        schema_version: 1,
        request_hash: f.metadata.request_hash.clone(),
        obligations_hash: f.metadata.obligations_hash.clone(),
        obligations: f.request.obligations.clone(),
    };
    let choices = acquisition_choices::AcquisitionChoices::derive(
        &contract,
        &f.metadata,
        "task",
        &f.ledger,
        &[],
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    let acquisition_choices::AcquisitionAction::QueryContext { query, .. } =
        choices.entries()[0].action.clone()
    else {
        panic!("expected obligation query")
    };
    let input = f
        .build(
            role,
            &usage(CompletionCallMode::Choice),
            CompletionCallMode::Choice,
            0,
            None,
        )
        .unwrap();
    assert!(!input.is_maximum_obligation_query(&query));
    let input = input
        .with_acquisition_choices(std::sync::Arc::new(choices))
        .unwrap();
    assert_eq!(query.max_bytes, MAX_QUERY_BYTES);
    assert_eq!(query.max_facts, 1);
    assert!(input.is_maximum_obligation_query(&query));
    let mut wider = query.clone();
    wider.max_facts = 2;
    assert!(!input.is_maximum_obligation_query(&wider));
    let mut smaller = query.clone();
    smaller.max_bytes -= 1;
    assert!(!input.is_maximum_obligation_query(&smaller));
    let mut other = query;
    other.scope = InvestigationScope::Node {
        id: "not-offered".into(),
    };
    assert!(!input.is_maximum_obligation_query(&other));
}
