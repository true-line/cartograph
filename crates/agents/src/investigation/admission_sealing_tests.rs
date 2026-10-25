//! Actual firewall preparation without provider execution or journal authority.
use super::*;
use llm::{
    EgressFirewall, EgressPolicy, OllamaProvider,
    bounded::{CompletionLimits, PreparedBoundedCompletion},
};
use staged_finish::{CompletionOutcome, StagedFinish};

fn prepare(input: &InvestigationInput) -> PreparedBoundedCompletion {
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    EgressFirewall::new(EgressPolicy::default())
        .prepare_bounded(
            &provider,
            &input.action("step:admission".into()).unwrap(),
            &CompletionLimits::default(),
        )
        .unwrap()
}
fn seal(input: &InvestigationInput) -> InvestigationDisclosure {
    input
        .seal_disclosure_with_policy(&prepare(input), &INVESTIGATION_V2_POLICY)
        .unwrap()
        .unwrap()
}

fn rehash(result: &mut InvestigationResult) {
    result.result_id.clear();
    let domain = match result.schema_version {
        6 => b"cartograph:investigation-result:v6\0".as_slice(),
        7 => b"cartograph:investigation-result:v7\0".as_slice(),
        _ => unreachable!(),
    };
    result.result_id = format!(
        "investigation-result-v{}:{}",
        result.schema_version,
        crate::source_basis::domain_hash(domain, &bounded_bytes(result, MAX_RESULT_BYTES).unwrap())
    );
}

#[test]
fn admission_sealing_binds_both_roles_modes_and_explicit_null_feedback() {
    // AC-0287: actual schema9 receipt, distinct from a synthetic sizing maximum.
    for role in [
        SpecialistId::DomainAnalystV10,
        SpecialistId::EvidenceAuditorV10,
    ] {
        let fixture = Fixture::new(role, 7);
        for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
            for reason in [
                None,
                Some(CapacityRefusalReason::PromptCapacity),
                Some(CapacityRefusalReason::FinishCallReserve),
                Some(CapacityRefusalReason::FinishTokenReserve),
                Some(CapacityRefusalReason::UnprovenContribution),
            ] {
                let feedback = reason.map(feedback);
                let input = fixture
                    .build(role, &usage(mode), mode, 1, feedback.as_ref())
                    .unwrap();
                let disclosure = seal(&input);
                // AC-0327: actual schema9 output in both modes/all feedbacks.
                let (retained, _) = InvestigationInput::sealing_workspace_bytes(
                    &INVESTIGATION_V2_POLICY,
                    &CompletionLimits::default(),
                )
                .unwrap();
                assert!(
                    retained
                        >= context_hub::logical::json_bytes(
                            context_hub::logical::count(&disclosure).unwrap(),
                        )
                        .unwrap()
                );
                assert_eq!(disclosure.schema_version, 6);
                assert!(
                    disclosure
                        .fingerprint_with_policy(input.ledger(), &INVESTIGATION_V2_POLICY)
                        .unwrap()
                        .starts_with("investigation-disclosure-v6:")
                );
                let stamp = disclosure.admission.as_ref().unwrap();
                assert_eq!(stamp.mode, mode);
                assert_eq!(stamp.consumed_budgets, usage(mode));
                assert_eq!(stamp.capacity_feedback, feedback);
                assert_eq!(stamp.capacity_refusal_count, 1);
                stamp
                    .validate_with_metadata(&fixture.metadata, &INVESTIGATION_V2_POLICY)
                    .unwrap();
                let raw = serde_json::to_value(stamp).unwrap();
                if reason.is_none() {
                    assert!(raw["capacity_feedback"].is_null());
                }
                let decoded: DisclosedAdmissionState = serde_json::from_value(raw.clone()).unwrap();
                assert_eq!(&decoded, stamp);
                let mut missing = raw;
                missing.as_object_mut().unwrap().remove("capacity_feedback");
                assert!(serde_json::from_value::<DisclosedAdmissionState>(missing).is_err());
                let mut old = disclosure.clone();
                old.schema_version = 5;
                assert!(
                    old.validate_with_policy(input.ledger(), &INVESTIGATION_V2_POLICY)
                        .is_err()
                );
                let mut missing = disclosure;
                missing.admission = None;
                assert!(
                    missing
                        .validate_with_policy(input.ledger(), &INVESTIGATION_V2_POLICY)
                        .is_err()
                );
            }
        }
    }
}

#[test]
fn admission_sealing_rejects_mutated_mode_phase_instructions_and_sizing_action() {
    // AC-0287: shape agreement and actual preparation identity are both required.
    let role = SpecialistId::DomainAnalystV10;
    let fixture = Fixture::new(role, 7);
    let input = fixture
        .build(
            role,
            &usage(CompletionCallMode::Choice),
            CompletionCallMode::Choice,
            0,
            None,
        )
        .unwrap();
    for field in [
        "admission_mode",
        "phase",
        "admission_instructions",
        "capacity_refusal_count",
        "capacity_feedback",
        "consumed_budgets",
    ] {
        let mut changed = input.clone();
        let mut body = prompt(&changed);
        match field {
            "admission_mode" => body["turn_progress"][field] = "finish_only".into(),
            "phase" => body["turn_progress"][field] = "finish_only".into(),
            "admission_instructions" => body["turn_progress"][field] = "extra authority".into(),
            "capacity_refusal_count" => body["turn_progress"][field] = 65.into(),
            "capacity_feedback" => {
                body["turn_progress"][field] =
                    serde_json::to_value(feedback(CapacityRefusalReason::PromptCapacity)).unwrap()
            }
            "consumed_budgets" => body[field]["model_invocations"] = 63.into(),
            _ => unreachable!(),
        }
        changed.payload.prompt = body.to_string();
        assert!(
            changed
                .seal_disclosure_with_policy(&prepare(&changed), &INVESTIGATION_V2_POLICY)
                .is_err(),
            "{field}"
        );
    }
    let mut changed = input.action("step:admission".into()).unwrap();
    let mut body: Value = serde_json::from_str(&changed.payload.prompt).unwrap();
    body["completion_metadata"]["completed_attempt_ids"] = json!(vec![
        "f".repeat(64);
        INVESTIGATION_V2_POLICY
            .max_completed_attempts()
    ]);
    body["turn_progress"]["capacity_refusal_count"] = 64.into();
    changed.payload.prompt = body.to_string();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let reserved = EgressFirewall::new(EgressPolicy::default())
        .prepare_bounded(&provider, &changed, &CompletionLimits::default())
        .unwrap();
    assert!(
        input
            .seal_disclosure_with_policy(&reserved, &INVESTIGATION_V2_POLICY)
            .is_err()
    );
}

#[test]
fn admission_sealing_rejects_actual_redaction_of_contract_and_citation_identity() {
    // AC-0287: fitting wire bytes cannot certify protected-field sealability.
    let role = SpecialistId::DomainAnalystV10;
    let policy = &INVESTIGATION_V2_POLICY;
    let mut fixture = Fixture::new(role, 7);
    let Obligation::Source {
        obligation_id,
        target,
    } = &mut fixture.request.obligations.obligations[0]
    else {
        unreachable!()
    };
    target.source.path = "ghp_abcdefgh".into();
    target.captured.file.path = target.source.path.clone();
    target.target_id = target.content_id().unwrap();
    *obligation_id = Obligation::Source {
        obligation_id: String::new(),
        target: target.clone(),
    }
    .content_id()
    .unwrap();
    fixture.metadata.request_hash = fixture.request.intent_hash_with_policy(policy).unwrap();
    fixture.metadata.obligations_hash = fixture
        .request
        .obligations
        .fingerprint_with_policy(policy)
        .unwrap();
    let input = fixture
        .build(
            role,
            &usage(CompletionCallMode::Choice),
            CompletionCallMode::Choice,
            0,
            None,
        )
        .unwrap();
    let prepared = prepare(&input);
    let actual: Value = serde_json::from_str(&prepared.preview().egress.payload.prompt).unwrap();
    assert_ne!(
        prompt(&input)["completion_contract"],
        actual["completion_contract"]
    );
    assert!(
        input
            .seal_disclosure_with_policy(&prepared, policy)
            .is_err()
    );

    let (mut fixture, raw, target, prototype) =
        source_fixture(role, "ordinary captured fixture text");
    let mut evidence = vec![];
    acquire_source(
        &mut fixture,
        &target,
        &prototype,
        &mut evidence,
        "ordinary captured fixture text",
    );
    fixture.ledger.citations.last_mut().unwrap().citation_id = "ghp_abcdefgh".into();
    evidence.last_mut().unwrap().id = "ghp_abcdefgh".into();
    fixture.basis.input_ledger_hash = fixture.ledger.fingerprint_with_policy(policy).unwrap();
    let input = build_source(&fixture, &raw, &evidence, role, CompletionCallMode::Choice);
    let prepared = prepare(&input);
    assert_ne!(
        input.payload.spans[0].id,
        prepared.preview().egress.payload.spans[0].id
    );
    assert!(
        input
            .seal_disclosure_with_policy(&prepared, policy)
            .is_err()
    );
}

#[test]
fn admission_sealing_mode_survives_receipt_roundtrip_at_every_finish_reserve_boundary() {
    // AC-0287: clock/caller choice cannot alter the mode derived from saved
    // pre-call usage. Host tests additionally authenticate that saved authority.
    let policy = &INVESTIGATION_V2_POLICY;
    for role in [
        SpecialistId::DomainAnalystV10,
        SpecialistId::EvidenceAuditorV10,
    ] {
        for refused in [false, true] {
            let mut fixture = Fixture::new(role, 7);
            if refused {
                fixture.metadata.refusal_count = 1;
                fixture.metadata.last_refusal_id = Some("b".repeat(64));
                fixture.metadata.feedback = Some(staged_finish::RefusalFeedback {
                    schema_version: 1,
                    unmet: vec![staged_finish::FeedbackEntry {
                        kind: "source".into(),
                        obligation_id: fixture.request.obligations.obligations[0].key().1.into(),
                        reason: staged_finish::RefusalReason::NotAttempted,
                    }],
                });
            }
            let reserve = fixture
                .metadata
                .finish_call_reserve_with_policy(policy)
                .unwrap();
            let per_call = policy.limits().generated_tokens_per_invocation;
            for calls in [
                reserve.model_invocations - 1,
                reserve.model_invocations,
                reserve.model_invocations + 1,
                reserve.model_invocations + 2,
            ] {
                for tokens in [
                    reserve.generated_token_reservations - 1,
                    reserve.generated_token_reservations,
                    reserve.generated_token_reservations + per_call - 1,
                    reserve.generated_token_reservations + per_call,
                    reserve.generated_token_reservations + per_call + 1,
                ] {
                    let mut usage = InvestigationUsage {
                        model_invocations: policy.limits().model_invocations - calls,
                        generated_token_reservations: policy.limits().generated_token_reservations
                            - tokens,
                        ..Default::default()
                    };
                    let expected = fixture.metadata.next_call_mode_with_policy(&usage, policy);
                    for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
                        let built = fixture.build(role, &usage, mode, 0, None);
                        assert_eq!(
                            built.is_ok(),
                            expected.as_ref().is_ok_and(|expected| *expected == mode)
                        );
                        if let Ok(input) = built {
                            let stamp = seal(&input).admission.unwrap();
                            let reopened: DisclosedAdmissionState =
                                serde_json::from_slice(&serde_json::to_vec(&stamp).unwrap())
                                    .unwrap();
                            reopened
                                .validate_with_metadata(&fixture.metadata, policy)
                                .unwrap();
                            assert_eq!(reopened.mode, mode);
                        }
                    }
                    usage.active_milliseconds = u64::MAX;
                    assert_eq!(
                        fixture.metadata.next_call_mode_with_policy(&usage, policy),
                        expected
                    );
                }
            }
        }
    }
}

#[test]
fn admission_sealing_result_seven_has_exact_disclosure_family_and_content_domain() {
    // AC-0287: supplied source/actual firewall receipt traverse production finish
    // admission; this pure fixture does not attest any durable host read.
    let role = SpecialistId::DomainAnalystV10;
    let policy = &INVESTIGATION_V2_POLICY;
    let text = "ordinary captured fixture text";
    let (mut fixture, raw, target, prototype) = source_fixture(role, text);
    let mut evidence = vec![];
    acquire_source(&mut fixture, &target, &prototype, &mut evidence, text);
    let input = build_source(&fixture, &raw, &evidence, role, CompletionCallMode::Choice);
    let disclosure = seal(&input);
    let finish = StagedFinish {
        schema_version: 1,
        outcome: CompletionOutcome::Supported,
        action: InvestigationAction::Finish {
            findings: vec![ProposedInvestigationFinding {
                claim_kind: InvestigationClaimKind::InferredInterpretation,
                title: "Scoped interpretation".into(),
                statement: "The retained material offers limited support for this interpretation."
                    .into(),
                citation_ids: vec![evidence[0].id.clone()],
                limitations: vec!["Only the retained scope was inspected.".into()],
            }],
            knowledge_completeness: KnowledgeCompleteness::Partial,
            limitations: vec!["No whole-project coverage is established.".into()],
        },
    };
    let result = input
        .admit_completion_finish_with_policy(
            &fixture.request,
            "task:admission",
            "step:admission",
            &disclosure,
            &finish,
            &[],
            None,
            "2026-10-21T12:00:00Z".into(),
            policy,
        )
        .unwrap()
        .ok()
        .expect("supplied source supports the scripted finish");
    assert_eq!(result.schema_version, 7);
    assert!(result.result_id.starts_with("investigation-result-v7:"));
    result
        .validate_completion_with_policy(
            &fixture.request,
            "task:admission",
            "step:admission",
            input.ledger(),
            &disclosure,
            &[],
            policy,
        )
        .unwrap();
    let mut old = result.clone();
    old.schema_version = 6;
    rehash(&mut old);
    assert!(
        old.validate_with_disclosure_and_policy(
            input.ledger(),
            "step:admission",
            &disclosure,
            policy
        )
        .is_err()
    );
    let mut wrong = disclosure.clone();
    wrong.admission = None;
    wrong.schema_version = 5;
    wrong.validate_with_policy(input.ledger(), policy).unwrap();
    let mut rehashed = result;
    rehashed.disclosure_hash = Some(
        wrong
            .fingerprint_with_policy(input.ledger(), policy)
            .unwrap(),
    );
    rehash(&mut rehashed);
    assert!(
        rehashed
            .validate_completion_with_policy(
                &fixture.request,
                "task:admission",
                "step:admission",
                input.ledger(),
                &wrong,
                &[],
                policy
            )
            .is_err()
    );
    let mut changed = disclosure;
    changed.admission.as_mut().unwrap().capacity_refusal_count = 1;
    // Pure shape can recognize a plausible invented count; only the host's
    // journal/cutoff authenticates it. Exact input binding still rejects it.
    changed
        .validate_with_policy(input.ledger(), policy)
        .unwrap();
    assert!(
        input
            .validate_completion_binding_with_policy(&fixture.request, &changed, &[], policy)
            .is_err()
    );
}
