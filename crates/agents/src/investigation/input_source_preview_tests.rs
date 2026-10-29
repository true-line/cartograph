//! AC-0287: Pure production serializers/firewall preparation with a dispatch-panicking
//! metadata-only fixture; no provider connection or source I/O.
use super::*;
use llm::bounded::*;
use llm::{CompletionPayload, Embedding, LlmProvider, Locality, ProviderCaps, ProviderError};

struct DescriptorOnly;
impl LlmProvider for DescriptorOnly {
    fn id(&self) -> &str {
        "source-preview-fixture"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        Ok(profile())
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        panic!("no dispatch")
    }
    fn complete_bounded(
        &self,
        _: &AuthorizedBoundedCompletion,
        _: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        panic!("no dispatch")
    }
}
fn profile() -> ProviderProfile {
    ProviderProfile {
        protocol_version: PROTOCOL_VERSION.into(),
        provider_id: "source-preview-fixture".into(),
        locality: Locality::Local,
        endpoint_id: "http://127.0.0.1:11434/api/chat".into(),
        requested_model: "frozen".into(),
    }
}
fn prepare(input: &InvestigationInput) -> PreparedBoundedCompletion {
    llm::EgressFirewall::new(llm::EgressPolicy::default())
        .prepare_bounded(
            &DescriptorOnly,
            &input.action("current:1".into()).unwrap(),
            &CompletionLimits::default(),
        )
        .unwrap()
}

#[test]
fn source_preview_proves_actual_source_append_and_disclosure_in_both_modes() {
    let policy = &INVESTIGATION_V2_POLICY;
    let limits = CompletionLimits::default();
    for role in [
        SpecialistId::DomainAnalystV10,
        SpecialistId::EvidenceAuditorV10,
        SpecialistId::DomainAnalystV16,
    ] {
        for text in [
            "é中🙂".to_string(),
            "secret=abcdefghijkl".into(),
            "\u{1}".repeat(8192),
        ] {
            let (mut fixture, raw, target, prototype) = source_fixture(role, &text);
            let mut evidence = vec![];
            for _ in 0..2 {
                let current =
                    build_source(&fixture, &raw, &evidence, role, CompletionCallMode::Choice);
                let prepared = prepare(&current);
                let proof = current.certify_unread_source_v9_with_policy(
                    &prepared,
                    &target,
                    &profile(),
                    &limits,
                    "next:2",
                    policy,
                );
                // AC-0454: the duplicated schema consumes real headroom. The
                // second maximal escaped source cannot prove wire closure at
                // unchanged limits; the first and all old-role cases still fit.
                if role == SpecialistId::DomainAnalystV16
                    && text == "\u{1}".repeat(8192)
                    && evidence.len() == 1
                {
                    assert_eq!(proof.err(), Some(InvestigationError::LimitExceeded));
                    assert_eq!(
                        current
                            .measure_unread_source_v9_with_policy(
                                &target,
                                &profile(),
                                &limits,
                                "next:2",
                                policy
                            )
                            .err(),
                        Some(InvestigationError::LimitExceeded)
                    );
                    break;
                }
                let proof = proof.unwrap();
                proof
                    .verify_with_policy(
                        &current,
                        &prepared,
                        &target,
                        &profile(),
                        &limits,
                        "next:2",
                        policy,
                    )
                    .unwrap();
                assert!(
                    proof
                        .verify_with_policy(
                            &current,
                            &prepared,
                            &target,
                            &profile(),
                            &limits,
                            "next:3",
                            policy
                        )
                        .is_err()
                );
                acquire_source(&mut fixture, &target, &prototype, &mut evidence, &text);
                for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
                    let actual = build_source(&fixture, &raw, &evidence, role, mode);
                    let measured = measure_bounded_action(
                        &profile(),
                        &actual.action("next:2".into()).unwrap(),
                        &limits,
                    )
                    .unwrap();
                    assert_dominates(
                        if mode == CompletionCallMode::Choice {
                            proof.continuation().choice
                        } else {
                            proof.continuation().finish_only
                        },
                        measured,
                    );
                    let disclosure = actual
                        .seal_disclosure_with_policy(&prepare(&actual), policy)
                        .unwrap()
                        .unwrap();
                    assert!(
                        serde_json::to_vec(&disclosure).unwrap().len()
                            <= proof.disclosure_upper_bytes()
                    );
                    assert!(proof.disclosure_upper_bytes() <= MAX_DISCLOSURE_BYTES);
                }
            }
        }
    }
}

#[test]
fn source_preview_rejects_fitting_wire_with_redacted_protected_contract() {
    let policy = &INVESTIGATION_V2_POLICY;
    let role = SpecialistId::DomainAnalystV10;
    let mut fixture = Fixture::new(role, 7);
    let Obligation::Source {
        obligation_id,
        target,
    } = &mut fixture.request.obligations.obligations[0]
    else {
        unreachable!()
    };
    target.source.path = "password=abcdefgh".into();
    target.captured.file.path = target.source.path.clone();
    target.target_id = target.content_id().unwrap();
    *obligation_id = Obligation::Source {
        obligation_id: String::new(),
        target: target.clone(),
    }
    .content_id()
    .unwrap();
    let target = *target.clone();
    fixture.metadata.request_hash = fixture.request.intent_hash_with_policy(policy).unwrap();
    fixture.metadata.obligations_hash = fixture
        .request
        .obligations
        .fingerprint_with_policy(policy)
        .unwrap();
    // A valid normalized contract fits every size gate; actual production
    // redaction nevertheless changes its protected source identity.
    fixture.request.normalized_with_policy(policy).unwrap();
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
    assert!(
        measure_bounded_action(
            &profile(),
            &input.action("current:1".into()).unwrap(),
            &CompletionLimits::default()
        )
        .is_ok()
    );
    assert_ne!(
        prepared.preview().egress.payload.prompt,
        input.payload.prompt
    );
    assert!(
        input
            .certify_unread_source_v9_with_policy(
                &prepared,
                &target,
                &profile(),
                &CompletionLimits::default(),
                "next:2",
                policy
            )
            .is_err()
    );
}

#[test]
fn source_preview_rejects_wrong_current_preparation_profile_and_descriptor() {
    let role = SpecialistId::DomainAnalystV10;
    let (fixture, raw, target, _) = source_fixture(role, "value");
    let input = build_source(&fixture, &raw, &[], role, CompletionCallMode::Choice);
    let prepared = prepare(&input);
    let proof = input
        .certify_unread_source_v9_with_policy(
            &prepared,
            &target,
            &profile(),
            &CompletionLimits::default(),
            "next:2",
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
    let different_current = llm::EgressFirewall::new(llm::EgressPolicy::default())
        .prepare_bounded(
            &DescriptorOnly,
            &input.action("different-current-step".into()).unwrap(),
            &CompletionLimits::default(),
        )
        .unwrap();
    assert!(
        proof
            .verify_with_policy(
                &input,
                &different_current,
                &target,
                &profile(),
                &CompletionLimits::default(),
                "next:2",
                &INVESTIGATION_V2_POLICY
            )
            .is_err()
    );
    let mut other_profile = profile();
    other_profile.requested_model = "different".into();
    assert!(
        input
            .certify_unread_source_v9_with_policy(
                &prepared,
                &target,
                &other_profile,
                &CompletionLimits::default(),
                "next:2",
                &INVESTIGATION_V2_POLICY
            )
            .is_err()
    );
    let mut other = input.clone();
    other.payload.prompt.push(' ');
    assert!(
        other
            .certify_unread_source_v9_with_policy(
                &prepared,
                &target,
                &profile(),
                &CompletionLimits::default(),
                "next:2",
                &INVESTIGATION_V2_POLICY
            )
            .is_err()
    );
    let mut changed = target.clone();
    changed.source.byte_end += 1;
    changed.target_id = changed.content_id().unwrap();
    assert!(
        input
            .certify_unread_source_v9_with_policy(
                &prepared,
                &changed,
                &profile(),
                &CompletionLimits::default(),
                "next:2",
                &INVESTIGATION_V2_POLICY
            )
            .is_err()
    );
}

#[test]
fn source_preview_protection_checks_span_metadata_but_allows_independent_text_redaction() {
    // Independently redacted source strings cannot reach protected prompt
    // fields, even when they contain private-key delimiters.
    let role = SpecialistId::DomainAnalystV10;
    let (fixture, raw, _, _) = source_fixture(role, "value");
    let input = build_source(&fixture, &raw, &[], role, CompletionCallMode::Choice);
    let mut payload: CompletionPayload = input.payload.clone();
    payload.spans.push(llm::PayloadSpan {
        id: "evidence-1".into(),
        repo: "r".into(),
        path: "p".into(),
        byte_start: 0,
        byte_end: 100,
        commit_sha: "abc".into(),
        text: "-----BEGIN PRIVATE KEY-----secret-----END PRIVATE KEY-----".into(),
    });
    let mut action = input.action("fixed".into()).unwrap();
    action.payload = payload.clone();
    let inspected =
        inspect_bounded_action(&profile(), &action, &CompletionLimits::default()).unwrap();
    assert_ne!(payload.spans[0].text, inspected.payload().spans[0].text);
    input::source_preview::protected_representation(input.ledger(), &payload, inspected.payload())
        .unwrap();
    let mut wrong = inspected.payload().clone();
    wrong.spans[0].path = "different".into();
    assert!(
        input::source_preview::protected_representation(input.ledger(), &payload, &wrong).is_err()
    );
}

#[test]
fn source_preview_disclosure_gate_accepts_exact_upper_size_and_rejects_one_less() {
    let role = SpecialistId::DomainAnalystV10;
    let (fixture, raw, _, _) = source_fixture(role, "value");
    let input = build_source(&fixture, &raw, &[], role, CompletionCallMode::Choice);
    let disclosure = input
        .seal_disclosure_with_policy(&prepare(&input), &INVESTIGATION_V2_POLICY)
        .unwrap()
        .unwrap();
    let bound =
        input::source_preview::disclosure_upper_bound(disclosure.clone(), 0, MAX_DISCLOSURE_BYTES)
            .unwrap();
    assert_eq!(
        input::source_preview::disclosure_upper_bound(disclosure.clone(), 0, bound).unwrap(),
        bound
    );
    assert_eq!(
        input::source_preview::disclosure_upper_bound(disclosure, 0, bound - 1),
        Err(InvestigationError::LimitExceeded)
    );
}

#[test]
fn source_preview_bound_covers_all_feedback_values_after_actual_append() {
    use staged_finish::{FeedbackEntry, RefusalFeedback, RefusalReason};
    let policy = &INVESTIGATION_V2_POLICY;
    let role = SpecialistId::DomainAnalystV10;
    let (mut fixture, raw, target, prototype) = source_fixture(role, "secret=abcdefgh");
    let initial = build_source(&fixture, &raw, &[], role, CompletionCallMode::Choice);
    let proof = initial
        .certify_unread_source_v9_with_policy(
            &prepare(&initial),
            &target,
            &profile(),
            &CompletionLimits::default(),
            "next:2",
            policy,
        )
        .unwrap();
    let mut evidence = vec![];
    acquire_source(
        &mut fixture,
        &target,
        &prototype,
        &mut evidence,
        "secret=abcdefgh",
    );
    fixture.metadata.refusal_count = 1;
    fixture.metadata.last_refusal_id = Some("0".repeat(64));
    fixture.metadata.completed_attempt_ids = (0..policy.max_completed_attempts())
        .map(|i| format!("{i:064x}"))
        .collect();
    for reason in [
        RefusalReason::NotAttempted,
        RefusalReason::AcquiredNotSupplied,
        RefusalReason::Redacted,
        RefusalReason::HashMismatch,
        RefusalReason::Unavailable,
    ] {
        fixture.metadata.feedback = Some(RefusalFeedback {
            schema_version: 1,
            unmet: fixture
                .request
                .obligations
                .obligations
                .iter()
                .map(|item| {
                    let (kind, id) = item.key();
                    FeedbackEntry {
                        obligation_id: id.into(),
                        kind: kind.into(),
                        reason,
                    }
                })
                .collect(),
        });
        for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
            let mut consumed = usage(mode);
            if mode == CompletionCallMode::FinishOnly {
                consumed.model_invocations = policy.limits().model_invocations - 1;
                consumed.generated_token_reservations =
                    policy.limits().generated_token_reservations - 2048;
            }
            consumed.reported_input_tokens = Some(u64::MAX);
            consumed.reported_output_tokens = Some(u64::MAX);
            // AC-0418: reserve a present cache key even when the baseline omits it.
            consumed.reported_cache_read_tokens = Some(u64::MAX);
            consumed.active_milliseconds = u64::MAX;
            for capacity in [
                CapacityRefusalReason::PromptCapacity,
                CapacityRefusalReason::FinishCallReserve,
                CapacityRefusalReason::FinishTokenReserve,
                CapacityRefusalReason::UnprovenContribution,
            ] {
                let actual = InvestigationInput::prepare_v9_with_policy(
                    &fixture.request.question,
                    role,
                    fixture.ledger.clone(),
                    &[&raw],
                    &evidence,
                    &[],
                    &consumed,
                    &fixture.basis,
                    &fixture.request,
                    &fixture.metadata,
                    &[],
                    mode,
                    consumed.model_invocations,
                    Some(&feedback(capacity)),
                    policy,
                )
                .unwrap();
                let measured = measure_bounded_action(
                    &profile(),
                    &actual.action("next:2".into()).unwrap(),
                    &CompletionLimits::default(),
                )
                .unwrap();
                assert_dominates(
                    if mode == CompletionCallMode::Choice {
                        proof.continuation().choice
                    } else {
                        proof.continuation().finish_only
                    },
                    measured,
                );
                let actual = actual
                    .seal_disclosure_with_policy(&prepare(&actual), policy)
                    .unwrap()
                    .unwrap();
                assert!(
                    serde_json::to_vec(&actual).unwrap().len() <= proof.disclosure_upper_bytes()
                );
            }
        }
    }
}

#[test]
fn source_preview_wire_gate_rejects_one_byte_below_full_continuation_bound() {
    let role = SpecialistId::DomainAnalystV10;
    let (fixture, raw, target, _) = source_fixture(role, "\u{1}".repeat(8192).as_str());
    let input = build_source(&fixture, &raw, &[], role, CompletionCallMode::Choice);
    let initial = input
        .certify_unread_source_v9_with_policy(
            &prepare(&input),
            &target,
            &profile(),
            &CompletionLimits::default(),
            "next:2",
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
    let sizes = initial.continuation();
    let exact_input = [sizes.choice, sizes.finish_only]
        .iter()
        .flat_map(|v| {
            [
                v.raw_string_bytes,
                v.original_payload_bytes,
                v.redacted_payload_bytes,
                v.original_action_bytes.saturating_sub(4096),
            ]
        })
        .max()
        .unwrap();
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let action = input.action("current:1".into()).unwrap();
    for (bytes, expected) in [(exact_input, true), (exact_input - 1, false)] {
        let limits = CompletionLimits::new(CompletionLimitValues {
            input_bytes: bytes,
            ..CompletionLimits::default().values()
        })
        .unwrap();
        let prepared = firewall
            .prepare_bounded(&DescriptorOnly, &action, &limits)
            .unwrap();
        assert_eq!(
            input
                .certify_unread_source_v9_with_policy(
                    &prepared,
                    &target,
                    &profile(),
                    &limits,
                    "next:2",
                    &INVESTIGATION_V2_POLICY
                )
                .is_ok(),
            expected
        );
    }
}
