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
                    citation_drop: None,
                    prose_correction: None,
                    schema_version: 1,
                    citation_correction: None,
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
        narrative: None,
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
    // AC-0287 / #1705: lower-level projection binding only. The host must
    // independently authorize @25 and prove the original/prior response hashes.
    let completion = result.completion.as_ref().unwrap();
    let witness = FindingWithholdingWitness {
        schema_version: 1,
        detector_version: 1,
        disposition: WithholdingDisposition::FindingsWithheldAfterCorrection,
        original_finish_hash: "1".repeat(64),
        original_response_hash: "2".repeat(64),
        request_hash: completion.request_hash.clone(),
        obligations_hash: completion.obligations_hash.clone(),
        input_ledger_hash: result.input_ledger_hash.clone(),
        disclosure_hash: result.disclosure_hash.clone().unwrap(),
        prior_correction_witness_hash: "3".repeat(64),
        projected_result_hash: result.projected_result_hash().unwrap(),
        original_finding_count: 2,
        withheld_indexes: vec![0],
        survivor_indexes: vec![1],
    };
    let partial = result
        .clone()
        .with_withholding_witness(witness, policy)
        .unwrap();
    assert_eq!(
        partial.completion.as_ref().unwrap().outcome,
        CompletionOutcome::FindingsWithheldAfterCorrection
    );
    assert_ne!(partial.result_id, result.result_id);
    partial
        .validate_specialist(SpecialistId::DomainAnalystV25)
        .unwrap();
    assert!(
        partial
            .validate_specialist(SpecialistId::DomainAnalystV24)
            .is_err()
    );
    let reopened: InvestigationResult =
        serde_json::from_slice(&serde_json::to_vec(&partial).unwrap()).unwrap();
    reopened
        .validate_with_disclosure_and_policy(input.ledger(), "step:admission", &disclosure, policy)
        .unwrap();
    assert_eq!(reopened.result_id, partial.result_id);
    let mut changed = partial.clone();
    changed.findings[0].statement = "A different interpretation was substituted.".into();
    rehash(&mut changed);
    assert!(
        changed
            .validate_with_disclosure_and_policy(
                input.ledger(),
                "step:admission",
                &disclosure,
                policy,
            )
            .is_err()
    );
    let mut unmet = partial.clone();
    unmet.completion.as_mut().unwrap().assessment[0].exactly_supplied = false;
    rehash(&mut unmet);
    assert!(
        unmet
            .validate_with_disclosure_and_policy(
                input.ledger(),
                "step:admission",
                &disclosure,
                policy,
            )
            .is_err()
    );
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

// AC-0473: literal legacy bytes, prompt pins and actual positive seal controls.
#[test]
fn reserved_acquisition_stamp_preserves_legacy_bytes_and_rejects_present() {
    const LEGACY: &[u8] = br#"{"schema_version":1,"mode":"choice","consumed_budgets":{"model_invocations":0,"tool_actions":0,"selected_facts":0,"evidence_requests":0,"evidence_items":0,"evidence_bytes":0,"captured_validation_bytes":0,"generated_token_reservations":0,"reported_input_tokens":null,"reported_output_tokens":null,"active_milliseconds":0},"capacity_refusal_count":0,"capacity_feedback":null,"turn_progress_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#;
    let legacy: DisclosedAdmissionState = serde_json::from_slice(LEGACY).unwrap();
    assert!(legacy.acquisition.is_none());
    legacy
        .validate_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    assert_eq!(serde_json::to_vec(&legacy).unwrap(), LEGACY);
    for (role, fingerprint) in [
        (
            SpecialistId::DomainAnalystV15,
            "ec99aee7d993cae30220094c8afb31430ba33a95b77cd4389ae81fbe0ef0041c",
        ),
        (
            SpecialistId::DomainAnalystV17,
            "3bbbb030e6c75e5d6b7adf9a00ff26597da61a772f4f77c384e11fdc348902a2",
        ),
    ] {
        assert_eq!(role.definition().unwrap().prompt_fingerprint, fingerprint);
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
        let disclosure = seal(&input);
        assert!(disclosure.admission.unwrap().acquisition.is_none());
        // Preserve the original admission and private target authority when
        // exercising the low-level sealer directly.
        let mut admission = input
            .disclosed_admission_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        assert!(admission.acquisition.is_none());
        assert!(
            serde_json::to_value(&admission)
                .unwrap()
                .get("acquisition")
                .is_none()
        );
        let action = input.action("step:admission".into()).unwrap();
        let prepared = prepare(&input);
        assert!(
            sealing::seal_with_admission_policy(
                input.ledger(),
                &action.payload,
                &prepared,
                &INVESTIGATION_V2_POLICY,
                input.inspection_requirements.as_deref(),
                Some(admission.clone())
            )
            .is_ok()
        );
        let reserved = AcquisitionAdmission {
            finish_patch: None,
            finish_policy_complete: false,
            capacity_exhausted: false,
            finish_reason: None,
            choices_hash: "b".repeat(64),
            action_feedback_hash: Some("c".repeat(64)),
        };
        reserved.validate().unwrap();
        admission.acquisition = Some(reserved);
        assert_eq!(
            admission.validate_with_policy(&INVESTIGATION_V2_POLICY),
            Err(InvestigationError::InvalidInput)
        );
        assert!(
            sealing::seal_with_admission_policy(
                input.ledger(),
                &action.payload,
                &prepared,
                &INVESTIGATION_V2_POLICY,
                input.inspection_requirements.as_deref(),
                Some(admission)
            )
            .is_err(),
            "{role:?}"
        );
    }
}

// AC-0473: hash shape is consistency only, never support for a reserved stamp.
#[test]
fn reserved_acquisition_stamp_checks_hash_shape() {
    for (choices_hash, action_feedback_hash, valid) in [
        ("a".repeat(64), None, true),
        ("a".repeat(64), Some("b".repeat(64)), true),
        ("short".into(), None, false),
        ("a".repeat(64), Some("not-a-hash".into()), false),
    ] {
        assert_eq!(
            AcquisitionAdmission {
                finish_patch: None,
                finish_policy_complete: false,
                capacity_exhausted: false,
                finish_reason: None,
                choices_hash,
                action_feedback_hash
            }
            .validate()
            .is_ok(),
            valid
        );
    }
    assert!(
        serde_json::from_value::<AcquisitionAdmission>(
            json!({"choices_hash":"a".repeat(64),"action_feedback_hash":null,"extra":true})
        )
        .is_err()
    );
}

#[test]
fn acquisition_finish_consistency_binds_identity_policy_and_stamp() {
    // AC-0491: these are pure caller witnesses, never durable authentication.
    let policy = &INVESTIGATION_V2_POLICY;
    for role in [
        SpecialistId::DomainAnalystV10,
        SpecialistId::EvidenceAuditorV10,
        SpecialistId::DomainAnalystV11,
        SpecialistId::DomainAnalystV12,
        SpecialistId::DomainAnalystV13,
        SpecialistId::DomainAnalystV14,
        SpecialistId::DomainAnalystV15,
        SpecialistId::DomainAnalystV17,
        SpecialistId::EvidenceAuditorV11,
        SpecialistId::DomainAnalystV18,
    ] {
        let text = "ordinary captured fixture material";
        let (mut fixture, raw, target, prototype) = source_fixture(role, text);
        let mut evidence = vec![];
        acquire_source(&mut fixture, &target, &prototype, &mut evidence, text);
        let input = build_source(&fixture, &raw, &evidence, role, CompletionCallMode::Choice);
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: fixture.metadata.request_hash.clone(),
            obligations_hash: fixture.metadata.obligations_hash.clone(),
            obligations: fixture.request.obligations.clone(),
        };
        let choices = acquisition_choices::AcquisitionChoices::derive(
            &contract,
            &fixture.metadata,
            "task:admission",
            input.ledger(),
            &[],
            policy,
        )
        .unwrap();
        let input = if role == SpecialistId::DomainAnalystV18 {
            input
                .with_acquisition_choices(std::sync::Arc::new(choices.clone()))
                .unwrap()
        } else {
            input
        };
        let disclosure = seal(&input);
        let finish = StagedFinish {
            narrative: None,
            schema_version: 1,
            outcome: CompletionOutcome::Supported,
            action: InvestigationAction::Finish {
                findings: vec![ProposedInvestigationFinding {
                    claim_kind: InvestigationClaimKind::InferredInterpretation,
                    title: "Scoped interpretation".into(),
                    statement:
                        "The retained material offers limited support for this interpretation."
                            .into(),
                    citation_ids: vec![evidence[0].id.clone()],
                    limitations: vec!["Only the retained scope was inspected.".into()],
                }],
                knowledge_completeness: KnowledgeCompleteness::Partial,
                limitations: vec!["Only this scoped material was considered.".into()],
            },
        };
        let generic = input.admit_completion_finish_with_policy(
            &fixture.request,
            "task:admission",
            "step:admission",
            &disclosure,
            &finish,
            &[],
            None,
            "2026-10-30T12:00:00Z".into(),
            policy,
        );
        if role != SpecialistId::DomainAnalystV18 {
            assert!(
                matches!(generic, Ok(Ok(_))),
                "{role:?}: expected a historical typed finish"
            );
            continue;
        }
        assert!(generic.is_err(), "unbound Some must remain rejected");
        let bound = choices
            .bind_completion_disclosure(
                "task:admission",
                "step:admission",
                &contract,
                &fixture.metadata,
                input.ledger(),
                &disclosure,
                None,
                policy,
            )
            .unwrap();
        let result = bound
            .admit_finish(
                &input,
                &fixture.request,
                &finish,
                &[],
                None,
                "2026-10-30T12:00:00Z".into(),
            )
            .unwrap()
            .ok()
            .expect("bound typed finish");
        bound
            .validate_completion(
                &result,
                &contract.request_hash,
                &contract.obligations,
                &fixture.request.expected_context_id,
                fixture.request.expected_graph_revision.as_deref(),
                &[],
            )
            .unwrap();
        for field in ["task", "step", "disclosure"] {
            let mut changed = result.clone();
            match field {
                "task" => changed.investigation_id = "task:foreign".into(),
                "step" => changed.invocation_step_id = Some("step:foreign".into()),
                "disclosure" => changed.disclosure_hash = Some("f".repeat(64)),
                _ => unreachable!(),
            }
            assert!(bound.validate_result(&changed).is_err(), "{field}");
        }
        for (task, step) in [
            ("task:foreign", "step:admission"),
            ("task:admission", "step:foreign"),
        ] {
            assert!(
                bound
                    .check(task, step, input.ledger(), &disclosure)
                    .is_err()
            );
        }
        let mut ledger = input.ledger().clone();
        ledger.revision += 1;
        assert!(
            bound
                .check("task:admission", "step:admission", &ledger, &disclosure)
                .is_err()
        );
        assert!(
            disclosure
                .validate_consistent(input.ledger(), &INVESTIGATION_V1_POLICY, Some(&bound))
                .is_err()
        );
        for field in ["choices", "feedback"] {
            let mut changed = disclosure.clone();
            let stamp = changed
                .admission
                .as_mut()
                .unwrap()
                .acquisition
                .as_mut()
                .unwrap();
            if field == "choices" {
                stamp.choices_hash = "a".repeat(64);
            } else {
                stamp.action_feedback_hash = Some("b".repeat(64));
            }
            assert!(
                choices
                    .bind_completion_disclosure(
                        "task:admission",
                        "step:admission",
                        &contract,
                        &fixture.metadata,
                        input.ledger(),
                        &changed,
                        None,
                        policy
                    )
                    .is_err(),
                "{field}"
            );
        }
    }
}

// AC-0260: finish witnesses require a feedback binding; absence stays historical.
#[test]
fn repeated_read_finish_stamp_preserves_legacy_bytes_and_rejects_other_reasons() {
    let mut stamp = AcquisitionAdmission {
        finish_patch: None,
        finish_policy_complete: false,
        capacity_exhausted: false,
        finish_reason: None,
        choices_hash: "a".repeat(64),
        action_feedback_hash: None,
    };
    let legacy = serde_json::to_value(&stamp).unwrap();
    assert!(legacy.get("finish_reason").is_none());
    assert_eq!(
        serde_json::from_value::<AcquisitionAdmission>(legacy).unwrap(),
        stamp
    );
    stamp.finish_reason = Some(ActionRefusalReason::RepeatedRead);
    assert!(stamp.validate().is_err());
    stamp.action_feedback_hash = Some("b".repeat(64));
    assert!(stamp.validate().is_ok());
    stamp.finish_reason = Some(ActionRefusalReason::UnknownAnchor);
    assert!(stamp.validate().is_err());
}

// AC-0566: the sealed invocation binds correction authentication to its exact
// choices/metadata. This is a pure witness test, not durable journal replay.
#[test]
fn citation_correction_authentication_uses_sealed_invocation_basis() {
    let role = SpecialistId::DomainAnalystV20;
    let policy = &INVESTIGATION_V2_POLICY;
    let f = Fixture::new(role, 7);
    let contract = InvestigationCompletionContract {
        schema_version: 1,
        request_hash: f.metadata.request_hash.clone(),
        obligations_hash: f.metadata.obligations_hash.clone(),
        obligations: f.request.obligations.clone(),
    };
    let Obligation::Source { target, .. } = &contract.obligations.obligations[0] else {
        panic!("source fixture")
    };
    let ids = vec![target.target_id.clone()];
    let choices = acquisition_choices::AcquisitionChoices::derive(
        &contract,
        &f.metadata,
        "task:admission",
        &f.ledger,
        &[],
        policy,
    )
    .unwrap();
    let mode = CompletionCallMode::Choice;
    let input = f
        .build(role, &usage(mode), mode, 0, None)
        .unwrap()
        .with_acquisition_choices(std::sync::Arc::new(choices.clone()))
        .unwrap();
    use llm::LlmProvider;
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let profile = provider.bounded_profile().unwrap();
    let limits = CompletionLimits::default();
    let reserve = input
        .measure_v9_reserve_with_policy(&profile, &limits, "step:admission", policy)
        .unwrap();
    let actual = llm::bounded::measure_bounded_action(
        &profile,
        &input.action("step:admission".into()).unwrap(),
        &limits,
    )
    .unwrap();
    assert_dominates(reserve.choice, actual);
    let disclosure = seal(&input);
    let correction = choices
        .citation_correction(&ids, &contract, &f.metadata, &f.ledger, policy)
        .unwrap()
        .unwrap();
    let bound = choices
        .bind_completion_disclosure(
            "task:admission",
            "step:admission",
            &contract,
            &f.metadata,
            input.ledger(),
            &disclosure,
            None,
            policy,
        )
        .unwrap();
    let authenticated = bound
        .authenticate_citation_correction(&correction, &ids)
        .unwrap();
    assert_eq!(
        authenticated
            .project(&choices, &contract, &f.metadata, &f.ledger, policy)
            .unwrap(),
        Some(correction.clone())
    );
    assert!(
        bound
            .authenticate_citation_correction(&correction, &[])
            .is_err()
    );
    assert!(
        bound
            .authenticate_citation_correction(&correction, &["f".repeat(64)])
            .is_err()
    );
    let mut wire = serde_json::to_value(&correction).unwrap();
    wire["choices_hash"] = "e".repeat(64).into();
    let forged = serde_json::from_value(wire).unwrap();
    assert!(
        bound
            .authenticate_citation_correction(&forged, &ids)
            .is_err()
    );
    let mut changed = f.metadata.clone();
    changed.request_hash = "b".repeat(64);
    assert!(
        choices
            .bind_completion_disclosure(
                "task:admission",
                "step:admission",
                &contract,
                &changed,
                input.ledger(),
                &disclosure,
                None,
                policy,
            )
            .is_err()
    );
}

// Decision #1688: exhausted menus have an explicit @21-only finish witness.
#[test]
fn capacity_exhausted_analyst21_seals_finish_only_without_action_refusal() {
    let role = SpecialistId::DomainAnalystV21;
    let policy = &INVESTIGATION_V2_POLICY;
    let f = Fixture::new(role, 7);
    let contract = InvestigationCompletionContract {
        schema_version: 1,
        request_hash: f.metadata.request_hash.clone(),
        obligations_hash: f.metadata.obligations_hash.clone(),
        obligations: f.request.obligations.clone(),
    };
    let original = acquisition_choices::AcquisitionChoices::derive(
        &contract,
        &f.metadata,
        "task:admission",
        &f.ledger,
        &[],
        policy,
    )
    .unwrap();
    let refused = original
        .entries()
        .iter()
        .map(|entry| {
            let value = serde_json::to_value(&entry.action).unwrap();
            core_prov::content_hash(&bounded_bytes(&value, MAX_ACTION_BYTES).unwrap())
        })
        .collect();
    let choices = original
        .excluding_capacity_refusals(&contract, &f.metadata, &f.ledger, &refused, policy)
        .unwrap();
    assert!(choices.capacity_exhausted());
    assert!(!choices.attempts_complete());
    let input = f
        .build(
            role,
            &usage(CompletionCallMode::Choice),
            CompletionCallMode::Choice,
            0,
            None,
        )
        .unwrap()
        .with_acquisition_choices(std::sync::Arc::new(choices))
        .unwrap();
    let disclosure = seal(&input);
    let stamp = disclosure.admission.as_ref().unwrap();
    assert_eq!(stamp.mode, CompletionCallMode::FinishOnly);
    let acquisition = stamp.acquisition.as_ref().unwrap();
    assert!(acquisition.capacity_exhausted);
    assert!(acquisition.finish_reason.is_none());
    assert!(acquisition.action_feedback_hash.is_none());
    assert!(stamp.validate_saved_role_shape(role, policy).is_ok());
    assert!(
        stamp
            .validate_saved_role_shape(SpecialistId::DomainAnalystV20, policy)
            .is_err()
    );
    assert!(
        stamp
            .validate_saved_role_shape(SpecialistId::EvidenceAuditorV11, policy)
            .is_err()
    );
}

#[test]
fn shared_prompt14_preserves_frozen_role_through_reserve_and_sealing() {
    // AC-0287: identical prompt bytes cannot confer the opt-in @25 authority.
    let policy = &INVESTIGATION_V2_POLICY;
    for role in [
        SpecialistId::DomainAnalystV24,
        SpecialistId::DomainAnalystV25,
    ] {
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
        assert_eq!(
            input.payload.system,
            SpecialistId::DomainAnalystV24.prompt()
        );
        assert_eq!(input.frozen_specialist().unwrap(), role);
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: fixture.metadata.request_hash.clone(),
            obligations_hash: fixture.metadata.obligations_hash.clone(),
            obligations: fixture.request.obligations.clone(),
        };
        let choices = std::sync::Arc::new(
            acquisition_choices::AcquisitionChoices::derive(
                &contract,
                &fixture.metadata,
                "task",
                &fixture.ledger,
                &[],
                policy,
            )
            .unwrap(),
        );
        let input = input.with_acquisition_choices(choices).unwrap();
        let prepared = prepare(&input);
        input
            .measure_v9_reserve_with_policy(
                prepared.profile(),
                prepared.limits(),
                "step:admission",
                policy,
            )
            .unwrap();
        let disclosure = seal(&input);
        disclosure
            .validate_saved_role_shape(input.ledger(), policy, role)
            .unwrap();
        assert_eq!(input.frozen_specialist().unwrap(), role);
        let other = if role == SpecialistId::DomainAnalystV24 {
            SpecialistId::DomainAnalystV25
        } else {
            SpecialistId::DomainAnalystV24
        };
        assert!(
            fixture
                .build(
                    other,
                    &usage(CompletionCallMode::Choice),
                    CompletionCallMode::Choice,
                    0,
                    None
                )
                .is_err()
        );
    }
}
