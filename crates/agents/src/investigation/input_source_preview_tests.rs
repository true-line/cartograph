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
                let proof = current
                    .certify_unread_source_v9_with_policy(
                        &prepared,
                        &target,
                        &profile(),
                        &limits,
                        "next:2",
                        policy,
                    )
                    .unwrap();
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
    input::source_preview::protected_representation(
        input.ledger(),
        &payload,
        inspected.payload(),
        None,
    )
    .unwrap();
    let mut wrong = inspected.payload().clone();
    wrong.spans[0].path = "different".into();
    assert!(
        input::source_preview::protected_representation(input.ledger(), &payload, &wrong, None)
            .is_err()
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

// AC-0486: binding-valid synthetic observations exercise pure representation
// closure, not host journal authority or completed native source reads.
fn source_choices(
    fixture: &Fixture,
    input: InvestigationInput,
    records: &[staged_attempt::AttemptRecord],
    prior: &InvestigationInputLedger,
) -> InvestigationInput {
    input
        .with_acquisition_choices(derive_source_choices(fixture, records, prior))
        .unwrap()
}
fn derive_source_choices(
    fixture: &Fixture,
    records: &[staged_attempt::AttemptRecord],
    prior: &InvestigationInputLedger,
) -> std::sync::Arc<acquisition_choices::AcquisitionChoices> {
    let contract = InvestigationCompletionContract {
        schema_version: 1,
        request_hash: fixture.metadata.request_hash.clone(),
        obligations_hash: fixture.metadata.obligations_hash.clone(),
        obligations: fixture.request.obligations.clone(),
    };
    let evidence = records
        .iter()
        .map(|record| staged_attempt::AttemptEvidence {
            record,
            ledger: if record.input_revision == fixture.ledger.revision {
                &fixture.ledger
            } else {
                prior
            },
        })
        .collect::<Vec<_>>();
    let choices = acquisition_choices::AcquisitionChoices::derive(
        &contract,
        &fixture.metadata,
        "source-preview-task",
        &fixture.ledger,
        &evidence,
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    std::sync::Arc::new(choices)
}
fn source_record(
    f: &Fixture,
    target: &InvestigationInspectionTarget,
    step: &str,
    outcome: staged_attempt::AttemptOutcome,
) -> staged_attempt::AttemptRecord {
    let obligation = f
        .request
        .obligations
        .obligations
        .iter()
        .find(|item| {
            staged_attempt::ReadIntent::Source {
                fact: target.fact.clone(),
                role: target.role,
                index: target.index,
            }
            .matches(item)
        })
        .unwrap();
    let mut record = staged_attempt::AttemptRecord {
        schema_version: 1,
        attempt_id: String::new(),
        task_id: "source-preview-task".into(),
        request_hash: f.metadata.request_hash.clone(),
        obligations_hash: f.metadata.obligations_hash.clone(),
        obligation_id: obligation.key().1.into(),
        step_id: step.into(),
        read: staged_attempt::ReadIntent::Source {
            fact: target.fact.clone(),
            role: target.role,
            index: target.index,
        },
        input_ledger_hash: f
            .ledger
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap(),
        input_revision: f.ledger.revision,
        outcome,
    };
    record.attempt_id = record.content_id().unwrap();
    record
}

#[test]
fn source_preview_bounds_last_pending_source_fallback() {
    let policy = &INVESTIGATION_V2_POLICY;
    let role = SpecialistId::DomainAnalystV18;
    for acquired in [false, true] {
        let (mut f, raw, target, prototype) = source_fixture(role, "bounded source");
        let mut other = target.clone();
        other.index += 1;
        other.target_id = other.content_id().unwrap();
        let mut obligation = Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(other.clone()),
        };
        let id = obligation.content_id().unwrap();
        if let Obligation::Source { obligation_id, .. } = &mut obligation {
            *obligation_id = id;
        }
        f.request.obligations.obligations.push(obligation);
        f.request.obligations = f
            .request
            .obligations
            .normalized_with_policy(policy)
            .unwrap();
        f.metadata.request_hash = f.request.intent_hash_with_policy(policy).unwrap();
        f.metadata.obligations_hash = f
            .request
            .obligations
            .fingerprint_with_policy(policy)
            .unwrap();
        let mut records = (0..policy.max_completed_attempts() - 1)
            .map(|i| {
                source_record(
                    &f,
                    &other,
                    &format!("prior:{i}"),
                    staged_attempt::AttemptOutcome::Unavailable {
                        reason: staged_attempt::UnavailableReason::PinnedTargetAbsent,
                    },
                )
            })
            .collect::<Vec<_>>();
        f.metadata.completed_attempt_ids = records
            .iter()
            .map(|record| record.attempt_id.clone())
            .collect();
        let prior = f.ledger.clone();
        let current = source_choices(
            &f,
            build_source(&f, &raw, &[], role, CompletionCallMode::Choice),
            &records,
            &prior,
        );
        assert_eq!(
            current
                .acquisition_choices
                .as_ref()
                .unwrap()
                .stamp_hash()
                .unwrap(),
            "67dbaa375a25aa00d18c314605484120aae60de093161344cf229186537c87bf"
        );
        current.assert_retained_workspace(policy);
        let action = current.action("current:1".into()).unwrap();
        let inspected =
            inspect_bounded_action(&profile(), &action, &CompletionLimits::default()).unwrap();
        let schema = action.payload.response_schema.as_deref().unwrap();
        input::source_preview::protected_representation(
            current.ledger(),
            &action.payload,
            inspected.payload(),
            Some(schema),
        )
        .unwrap();
        assert!(
            input::source_preview::protected_representation(
                current.ledger(),
                &action.payload,
                inspected.payload(),
                Some("{}")
            )
            .is_err()
        );
        let mut altered = inspected.payload().clone();
        let mut body: Value = serde_json::from_str(&altered.prompt).unwrap();
        body["turn_progress"]["acquisition_choices"]["entries"] = json!([]);
        altered.prompt = serde_json::to_string(&body).unwrap();
        assert!(
            input::source_preview::protected_representation(
                current.ledger(),
                &action.payload,
                &altered,
                Some(schema)
            )
            .is_err()
        );
        let proof = current
            .certify_unread_source_v9_with_policy(
                &prepare(&current),
                &target,
                &profile(),
                &CompletionLimits::default(),
                "next:2",
                policy,
            )
            .unwrap();
        let boundary = [
            proof.continuation().choice,
            proof.continuation().finish_only,
        ]
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
        for (bytes, expected) in [(boundary, true), (boundary - 1, false)] {
            let limits = CompletionLimits::new(CompletionLimitValues {
                input_bytes: bytes,
                ..CompletionLimits::default().values()
            })
            .unwrap();
            let prepared = llm::EgressFirewall::new(llm::EgressPolicy::default())
                .prepare_bounded(&DescriptorOnly, &action, &limits)
                .unwrap();
            assert_eq!(
                current
                    .certify_unread_source_v9_with_policy(
                        &prepared,
                        &target,
                        &profile(),
                        &limits,
                        "next:2",
                        policy
                    )
                    .is_ok(),
                expected
            );
        }
        let mut evidence = vec![];
        if acquired {
            acquire_source(&mut f, &target, &prototype, &mut evidence, "bounded source");
        }
        let outcome = if acquired {
            staged_attempt::AttemptOutcome::SourceAcquired {
                citation_id: "evidence-1".into(),
            }
        } else {
            staged_attempt::AttemptOutcome::Unavailable {
                reason: staged_attempt::UnavailableReason::PinnedTargetAbsent,
            }
        };
        records.push(source_record(&f, &target, "next:2", outcome));
        f.metadata.completed_attempt_ids = records
            .iter()
            .map(|record| record.attempt_id.clone())
            .collect();
        for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
            let actual = source_choices(
                &f,
                build_source(&f, &raw, &evidence, role, mode),
                &records,
                &prior,
            );
            let action = actual.action("next:2".into()).unwrap();
            // Completed @18 choices retain @17 exploration except that replay
            // is narrowed to the exact durable references (#1623).
            let historical =
                response_schema::select_admitted(SpecialistId::DomainAnalystV17, mode, &f.ledger)
                    .unwrap()
                    .unwrap();
            let mut historical: Value = serde_json::from_str(&historical).unwrap();
            let mut current: Value =
                serde_json::from_str(action.payload.response_schema.as_deref().unwrap()).unwrap();
            for schema in [&mut historical, &mut current] {
                schema["anyOf"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|branch| branch["properties"]["type"]["const"] != "replay_query");
            }
            assert_eq!(current, historical);
            let measured =
                measure_bounded_action(&profile(), &action, &CompletionLimits::default()).unwrap();
            assert_dominates(
                if mode == CompletionCallMode::Choice {
                    proof.continuation().choice
                } else {
                    proof.continuation().finish_only
                },
                measured,
            );
        }
    }
}

#[test]
fn source_preview_projects_deferred_choices_without_authority() {
    let policy = &INVESTIGATION_V2_POLICY;
    let (mut f, _, template, _) = source_fixture(SpecialistId::DomainAnalystV18, "bounded");
    let mut sources = vec![];
    for index in 0..policy.max_completion_obligations() {
        let mut target = template.clone();
        target.fact = crate::TaskFactKey::Node {
            id: format!("{index:02}{}", "\"".repeat(1000)),
        };
        target.target_id = target.content_id().unwrap();
        let mut item = Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(target),
        };
        let id = item.content_id().unwrap();
        if let Obligation::Source { obligation_id, .. } = &mut item {
            *obligation_id = id;
        }
        sources.push(item);
        f.request.obligations.obligations = sources.clone();
        if f.request
            .obligations
            .normalized_with_policy(policy)
            .is_err()
        {
            sources.pop();
            break;
        }
    }
    f.request.obligations.obligations = sources;
    f.request.obligations = f
        .request
        .obligations
        .normalized_with_policy(policy)
        .unwrap();
    f.metadata.request_hash = f.request.intent_hash_with_policy(policy).unwrap();
    f.metadata.obligations_hash = f
        .request
        .obligations
        .fingerprint_with_policy(policy)
        .unwrap();
    let Obligation::Source { target, .. } = &f.request.obligations.obligations[0] else {
        unreachable!()
    };
    let target = target.clone();
    // Every target is already bound, so the deferred frontier contains source
    // reads too. Completing one read frees enough space for a deferred peer.
    let selected = f.ledger.selected_facts[0].clone();
    let citation = f.ledger.citations[0].clone();
    f.ledger.selected_facts.clear();
    f.ledger.citations.clear();
    for (index, obligation) in f.request.obligations.obligations.iter().enumerate() {
        let Obligation::Source { target, .. } = obligation else {
            unreachable!()
        };
        let mut fact = selected.clone();
        fact.fact = target.fact.clone();
        f.ledger.selected_facts.push(fact);
        let mut graph = citation.clone();
        graph.fact = target.fact.clone();
        graph.citation_id = format!("graph-{index}");
        f.ledger.citations.push(graph);
    }
    f.ledger.selected_facts.sort_by(|a, b| a.fact.cmp(&b.fact));
    let selected_count = f.ledger.selected_facts.len();
    f.ledger.queries[0].query.max_facts = selected_count;
    f.ledger.queries[0].returned_facts = selected_count;
    f.ledger.queries[0].total_selected = selected_count;
    let current = derive_source_choices(&f, &[], &f.ledger);
    assert!(current.deferred() > 0);
    assert!(
        current.private_bytes()
            <= acquisition_choices::AcquisitionChoices::private_workspace_bytes(policy).unwrap()
    );
    let contract = InvestigationCompletionContract {
        schema_version: 1,
        request_hash: f.metadata.request_hash.clone(),
        obligations_hash: f.metadata.obligations_hash.clone(),
        obligations: f.request.obligations.clone(),
    };
    let (projected, schema) = current
        .source_projection(
            &contract,
            &f.ledger,
            &target,
            CompletionCallMode::Choice,
            policy,
        )
        .unwrap();
    let record = source_record(
        &f,
        &target,
        "conditional-read",
        staged_attempt::AttemptOutcome::Unavailable {
            reason: staged_attempt::UnavailableReason::PinnedTargetAbsent,
        },
    );
    f.metadata.completed_attempt_ids = vec![record.attempt_id.clone()];
    let actual = derive_source_choices(&f, &[record], &f.ledger);
    assert!(actual.entries().iter().any(|entry| {
        !current
            .entries()
            .iter()
            .any(|old| old.obligation_id == entry.obligation_id)
    }));
    let mut expected = serde_json::to_value(&*actual).unwrap();
    expected["input_ledger_hash"] = "f".repeat(64).into();
    expected["completion_metadata_hash"] = "f".repeat(64).into();
    assert_eq!(projected, expected);
    assert_eq!(
        schema,
        response_schema::select_acquisition(
            CompletionCallMode::Choice,
            &actual,
            &f.ledger,
            &f.metadata.request_hash
        )
        .unwrap()
    );
    assert!(bounded_bytes(&projected, MAX_ACTION_BYTES).is_ok());
}

#[test]
fn analyst19_only_recovers_known_unacquired_citations_without_result() {
    // AC-0565: exercise real input assembly, sealing and staged admission. The
    // descriptor panics on dispatch; this test neither reads nor invokes a model.
    let policy = &INVESTIGATION_V2_POLICY;
    for (role, mode) in [
        (SpecialistId::DomainAnalystV18, CompletionCallMode::Choice),
        (SpecialistId::DomainAnalystV19, CompletionCallMode::Choice),
        (SpecialistId::DomainAnalystV20, CompletionCallMode::Choice),
        (
            SpecialistId::DomainAnalystV20,
            CompletionCallMode::FinishOnly,
        ),
        (
            SpecialistId::DomainAnalystV18,
            CompletionCallMode::FinishOnly,
        ),
        (
            SpecialistId::DomainAnalystV19,
            CompletionCallMode::FinishOnly,
        ),
    ] {
        let (mut f, raw, target, prototype) = source_fixture(role, "bounded source");
        let input = source_choices(&f, build_source(&f, &raw, &[], role, mode), &[], &f.ledger);
        let disclosure = input
            .seal_disclosure_with_policy(&prepare(&input), policy)
            .unwrap()
            .unwrap();
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: f.metadata.request_hash.clone(),
            obligations_hash: f.metadata.obligations_hash.clone(),
            obligations: f.request.obligations.clone(),
        };
        let saved_metadata = f.metadata.clone();
        let bound = input
            .acquisition_choices
            .as_ref()
            .unwrap()
            .bind_completion_disclosure(
                "source-preview-task",
                "current:1",
                &contract,
                &saved_metadata,
                input.ledger(),
                &disclosure,
                None,
                policy,
            )
            .unwrap();
        for foreign in [false, true] {
            let mut ids = vec![target.target_id.clone()];
            if foreign {
                ids.push("foreign-target".into());
            }
            let finish = staged_finish::StagedFinish {
                schema_version: 1,
                outcome: staged_finish::CompletionOutcome::Supported,
                narrative: None,
                action: InvestigationAction::Finish {
                    findings: vec![ProposedInvestigationFinding {
                        claim_kind: InvestigationClaimKind::InferredInterpretation,
                        title: "Candidate interpretation".into(),
                        statement: "A candidate interpretation requires a source check.".into(),
                        citation_ids: ids,
                        limitations: vec!["Only selected material was considered.".into()],
                    }],
                    knowledge_completeness: KnowledgeCompleteness::Partial,
                    limitations: vec!["Only selected material was considered.".into()],
                },
            };
            let result = staged_finish::admit_finish_with_policy_using_consistency(
                &f.request,
                "source-preview-task",
                "current:1",
                &input,
                &disclosure,
                &finish,
                &[],
                None,
                "2026-11-03T00:00:00Z".into(),
                policy,
                Some(&bound),
            );
            if matches!(
                role,
                SpecialistId::DomainAnalystV19 | SpecialistId::DomainAnalystV20
            ) && !foreign
                && mode == CompletionCallMode::Choice
            {
                let staged_finish::FinishDecision::Refused {
                    assessment,
                    feedback,
                } = result.unwrap()
                else {
                    panic!("unacquired target must produce feedback, never a result");
                };
                assert_eq!(
                    feedback.citation_correction.is_some(),
                    role == SpecialistId::DomainAnalystV20
                );
                assert_eq!(
                    feedback.schema_version,
                    if role == SpecialistId::DomainAnalystV20 {
                        2
                    } else {
                        1
                    }
                );
                assert!(
                    !feedback.unmet.is_empty(),
                    "eligible pending source must remain unmet"
                );
                let mut refusal = staged_refusal::RefusalRecord {
                    schema_version: 1,
                    refusal_id: String::new(),
                    task_id: "source-preview-task".into(),
                    request_hash: f.metadata.request_hash.clone(),
                    obligations_hash: f.metadata.obligations_hash.clone(),
                    step_id: "current:1".into(),
                    input_ledger_hash: f.ledger.fingerprint_with_policy(policy).unwrap(),
                    disclosure_hash: bound.fingerprint().unwrap(),
                    finish_hash: finish.fingerprint().unwrap(),
                    outcome: finish.outcome,
                    response_hash: core_prov::content_hash(b"synthetic correction response"),
                    previous_refusal: None,
                    refusal_count: 1,
                    assessment,
                    feedback,
                };
                refusal.refusal_id = refusal.content_id().unwrap();
                assert_eq!(
                    bound
                        .validate_refusal(
                            &refusal,
                            &f.metadata.request_hash,
                            &f.request.obligations,
                            &[],
                            None
                        )
                        .unwrap(),
                    staged_refusal::RefusalDisposition::Continue
                );
                // AC-0566: pure stored mapping validation at the original
                // cutoff. Host role/response journal checks are separate.
                let mut typed = refusal.clone();
                typed.feedback.schema_version = 2;
                typed.feedback.citation_correction = input
                    .acquisition_choices
                    .as_ref()
                    .unwrap()
                    .citation_correction(
                        std::slice::from_ref(&target.target_id),
                        &contract,
                        &saved_metadata,
                        input.ledger(),
                        policy,
                    )
                    .unwrap();
                typed.refusal_id = typed.content_id().unwrap();
                assert!(
                    bound
                        .validate_refusal(
                            &typed,
                            &contract.request_hash,
                            &contract.obligations,
                            &[],
                            None
                        )
                        .is_ok()
                );
                assert!(
                    typed
                        .validate_stored_basis_with_policy(
                            &contract.request_hash,
                            &contract.obligations,
                            input.ledger(),
                            &disclosure,
                            None,
                            policy
                        )
                        .is_err()
                );
                let mut changed = serde_json::to_value(&typed).unwrap();
                changed["feedback"]["citation_correction"]["choices_hash"] = "d".repeat(64).into();
                let mut forged: staged_refusal::RefusalRecord =
                    serde_json::from_value(changed).unwrap();
                forged.refusal_id = forged.content_id().unwrap();
                assert!(
                    bound
                        .validate_refusal(
                            &forged,
                            &contract.request_hash,
                            &contract.obligations,
                            &[],
                            None
                        )
                        .is_err()
                );
                let saved_correction = refusal.feedback.citation_correction.clone();
                let authenticated = saved_correction.as_ref().map(|correction| {
                    bound
                        .authenticate_citation_correction(
                            correction,
                            std::slice::from_ref(&target.target_id),
                        )
                        .unwrap()
                });
                f.metadata.refusal_count = 1;
                f.metadata.last_refusal_id = Some(refusal.refusal_id);
                f.metadata.feedback = Some(refusal.feedback);
                if role == SpecialistId::DomainAnalystV20 {
                    let choices = derive_source_choices(&f, &[], &f.ledger);
                    let base = build_source(&f, &raw, &[], role, CompletionCallMode::Choice);
                    assert!(
                        base.clone()
                            .with_acquisition_choices(choices.clone())
                            .is_err()
                    );
                    let next = base
                        .with_acquisition_correction(choices, None, authenticated.as_ref())
                        .unwrap();
                    let prepared = prepare(&next);
                    let reserve = input
                        .measure_v9_reserve_with_policy(
                            prepared.profile(),
                            prepared.limits(),
                            "current:1",
                            policy,
                        )
                        .unwrap();
                    let actual = llm::bounded::measure_bounded_action(
                        prepared.profile(),
                        &next.action("current:1".into()).unwrap(),
                        prepared.limits(),
                    )
                    .unwrap();
                    assert_dominates(reserve.choice, actual);
                    let prompt: serde_json::Value =
                        serde_json::from_str(&prepared.preview().egress.payload.prompt).unwrap();
                    assert_eq!(
                        prompt["turn_progress"]["citation_correction"]["entries"][0]["target_id"],
                        target.target_id
                    );
                    assert_eq!(
                        prompt["turn_progress"]["citation_correction"]["entries"][0]["action"]["type"],
                        "read_evidence"
                    );
                    let mut forged = next.clone();
                    let mut wire: serde_json::Value =
                        serde_json::from_str(&forged.payload.prompt).unwrap();
                    wire["turn_progress"]["citation_correction"]["entries"][0]["target_id"] =
                        "f".repeat(64).into();
                    forged.payload.prompt = wire.to_string();
                    assert!(forged.action("tampered".into()).is_err());
                }
                // This is a synthetic acquisition observation, not native I/O.
                // No citation exists until the explicit source acquisition below.
                let prior = f.ledger.clone();
                let mut evidence = vec![];
                acquire_source(&mut f, &target, &prototype, &mut evidence, "bounded source");
                let record = source_record(
                    &f,
                    &target,
                    "source:2",
                    staged_attempt::AttemptOutcome::SourceAcquired {
                        citation_id: "evidence-1".into(),
                    },
                );
                f.metadata.completed_attempt_ids = vec![record.attempt_id.clone()];
                let records = [record];
                let acquired = build_source(&f, &raw, &evidence, role, CompletionCallMode::Choice)
                    .with_acquisition_correction(
                        derive_source_choices(&f, &records, &prior),
                        None,
                        authenticated.as_ref(),
                    )
                    .unwrap();
                let disclosed = acquired
                    .seal_disclosure_with_policy(&prepare(&acquired), policy)
                    .unwrap()
                    .unwrap();
                let accepted_bound = acquired
                    .acquisition_choices
                    .as_ref()
                    .unwrap()
                    .bind_completion_disclosure(
                        "source-preview-task",
                        "current:1",
                        &contract,
                        &f.metadata,
                        acquired.ledger(),
                        &disclosed,
                        None,
                        policy,
                    )
                    .unwrap();
                let mut corrected = finish.clone();
                if let InvestigationAction::Finish { findings, .. } = &mut corrected.action {
                    findings[0].citation_ids = vec!["evidence-1".into()];
                }
                let attempts = [staged_attempt::AttemptEvidence {
                    record: &records[0],
                    ledger: &f.ledger,
                }];
                let admitted = accepted_bound
                    .admit_finish(
                        &acquired,
                        &f.request,
                        &corrected,
                        &attempts,
                        None,
                        "2026-11-03T00:00:01Z".into(),
                    )
                    .unwrap();
                assert!(
                    admitted.is_ok(),
                    "acquired and supplied source must allow a valid finish"
                );
            } else {
                assert_eq!(
                    result.err(),
                    Some(InvestigationError::InvalidCitation),
                    "role={role:?}, mode={mode:?}, foreign={foreign}"
                );
            }
            assert!(
                input
                    .ledger()
                    .citations
                    .iter()
                    .all(|citation| citation.citation_id != target.target_id)
            );
        }
    }
}

// AC-0566: only @20 accepts receipt handles through real input/sealing.
#[test]
fn analyst20_receipt_correction_is_role_bound_and_authenticated() {
    let policy = &INVESTIGATION_V2_POLICY;
    for role in [
        SpecialistId::DomainAnalystV19,
        SpecialistId::DomainAnalystV20,
    ] {
        let (f, raw, target, _) = source_fixture(role, "bounded source");
        let input = source_choices(
            &f,
            build_source(&f, &raw, &[], role, CompletionCallMode::Choice),
            &[],
            &f.ledger,
        );
        let disclosure = input
            .seal_disclosure_with_policy(&prepare(&input), policy)
            .unwrap()
            .unwrap();
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: f.metadata.request_hash.clone(),
            obligations_hash: f.metadata.obligations_hash.clone(),
            obligations: f.request.obligations.clone(),
        };
        let bound = input
            .acquisition_choices
            .as_ref()
            .unwrap()
            .bind_completion_disclosure(
                "source-preview-task",
                "current:1",
                &contract,
                &f.metadata,
                input.ledger(),
                &disclosure,
                None,
                policy,
            )
            .unwrap();
        let finish = staged_finish::StagedFinish {
            schema_version: 1,
            outcome: staged_finish::CompletionOutcome::Supported,
            narrative: None,
            action: InvestigationAction::Finish {
                findings: vec![ProposedInvestigationFinding {
                    claim_kind: InvestigationClaimKind::InferredInterpretation,
                    title: "Candidate interpretation".into(),
                    statement: "A candidate interpretation requires a source check.".into(),
                    citation_ids: vec![target.receipt_id.clone()],
                    limitations: vec!["Only selected material was considered.".into()],
                }],
                knowledge_completeness: KnowledgeCompleteness::Partial,
                limitations: vec!["Only selected material was considered.".into()],
            },
        };
        let correction = input
            .typed_citation_correction(&disclosure, &finish, None, policy, Some(&bound))
            .unwrap();
        assert_eq!(correction.is_some(), role == SpecialistId::DomainAnalystV20);
        if let Some(correction) = correction {
            bound
                .authenticate_citation_correction(&correction, &[target.target_id])
                .unwrap();
        }
    }
}

#[test]
fn analyst22_locator_discrepancy_preserves_refusal_and_real_errors() {
    let policy = &INVESTIGATION_V2_POLICY;
    let role = SpecialistId::DomainAnalystV22;
    let (f, raw, _, _) = source_fixture(role, "bounded source");
    let input = source_choices(
        &f,
        build_source(&f, &raw, &[], role, CompletionCallMode::Choice),
        &[],
        &f.ledger,
    );
    let disclosure = input
        .seal_disclosure_with_policy(&prepare(&input), policy)
        .unwrap()
        .unwrap();
    let contract = InvestigationCompletionContract {
        schema_version: 1,
        request_hash: f.metadata.request_hash.clone(),
        obligations_hash: f.metadata.obligations_hash.clone(),
        obligations: f.request.obligations.clone(),
    };
    let bound = input
        .acquisition_choices
        .as_ref()
        .unwrap()
        .bind_completion_disclosure(
            "source-preview-task",
            "current:1",
            &contract,
            &f.metadata,
            input.ledger(),
            &disclosure,
            None,
            policy,
        )
        .unwrap();
    let finish = staged_finish::StagedFinish {
        schema_version: 1,
        outcome: staged_finish::CompletionOutcome::InsufficientEvidence,
        narrative: None,
        action: InvestigationAction::Finish {
            findings: vec![],
            knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
            limitations: vec![
                input
                    .ledger()
                    .coverage
                    .as_ref()
                    .unwrap()
                    .qualification
                    .coverage_id
                    .clone(),
            ],
        },
    };
    assert_eq!(
        input.validate_correction_prose(&finish, None),
        Err(InvestigationError::SourceReplay)
    );
    // Provider metadata is not authored by the analyst and cannot be rewritten.
    let mut metadata_finish = finish.clone();
    let InvestigationAction::Finish { limitations, .. } = &mut metadata_finish.action else {
        unreachable!();
    };
    let observed_model = limitations.remove(0);
    limitations.push("Additional evidence is required.".into());
    assert_eq!(
        input.validate_correction_prose(&metadata_finish, None),
        Ok(())
    );
    let location = input
        .correction_replay_location(&metadata_finish, Some(&observed_model))
        .unwrap()
        .unwrap();
    assert_eq!(location.start.field, replay_location::ProseField::Model);
    let metadata_result = staged_finish::admit_finish_with_policy_using_consistency(
        &f.request,
        "source-preview-task",
        "current:1",
        &input,
        &disclosure,
        &metadata_finish,
        &[],
        Some(observed_model),
        "2026-11-06T00:00:00Z".into(),
        policy,
        Some(&bound),
    );
    assert!(matches!(
        metadata_result,
        Err(InvestigationError::SourceReplay)
    ));
    use replay_location::{ProseField, ProsePosition, ReplayLocation};
    let position = |field| ProsePosition { field, sentence: 1 };
    for diagnostic in [
        Ok(Some(ReplayLocation {
            start: position(ProseField::Model),
            end: position(ProseField::Narrative),
        })),
        Ok(Some(ReplayLocation {
            start: position(ProseField::Limitation { limitation: 0 }),
            end: position(ProseField::Model),
        })),
        Ok(None),
        Err(InvestigationError::SourceReplay),
        Err(InvestigationError::SensitiveOutput),
        Err(InvestigationError::LimitExceeded),
        Err(InvestigationError::InvalidAction),
    ] {
        let expected = diagnostic.clone();
        let called = std::cell::Cell::new(false);
        let result = staged_finish::admit_finish_with_locator(
            &f.request,
            "source-preview-task",
            "current:1",
            &input,
            &disclosure,
            &finish,
            &[],
            None,
            "2026-11-06T00:00:00Z".into(),
            policy,
            Some(&bound),
            |_, _, _| {
                called.set(true);
                diagnostic
            },
        );
        assert!(called.get());
        match expected {
            Ok(_) | Err(InvestigationError::SourceReplay) => {
                let staged_finish::FinishDecision::Refused { feedback, .. } = result.unwrap()
                else {
                    panic!("diagnostic discrepancy must preserve a recoverable refusal");
                };
                assert_eq!(feedback.schema_version, 3);
                assert_eq!(
                    feedback.prose_correction,
                    Some(staged_finish::ProseCorrection::ParaphraseWithoutQuotingSource)
                );
                assert!(!feedback.unmet.is_empty());
            }
            Err(error) => assert!(matches!(result, Err(actual) if actual == error)),
        }
    }
}

// AC-0287 / #1701: the measured claim is bound independently of its maximum.
#[test]
fn measured_source_preview_binds_both_sizes_and_rejects_legacy_verification() {
    let text = "secret=abcd";
    let role = SpecialistId::DomainAnalystV10;
    let (fixture, raw, target, _) = source_fixture(role, text);
    let current = build_source(&fixture, &raw, &[], role, CompletionCallMode::Choice);
    let prepared = prepare(&current);
    let sizes = core_redact::measure_direct_source_json_content(text).unwrap();
    let profile = profile();
    let limits = CompletionLimits::default();
    let policy = &INVESTIGATION_V2_POLICY;
    let proof = current
        .certify_measured_unread_source_v9_with_policy(
            &prepared, &target, &profile, &limits, "next:2", policy, sizes,
        )
        .unwrap();
    proof
        .verify_measured_with_policy(
            &current, &prepared, &target, &profile, &limits, "next:2", policy, sizes,
        )
        .unwrap();
    assert!(
        proof
            .verify_with_policy(
                &current, &prepared, &target, &profile, &limits, "next:2", policy,
            )
            .is_err()
    );
    let changed = core_redact::DirectSourceJsonContentSizes {
        original: sizes.original + 1,
        ..sizes
    };
    // Same max(original, redacted), different claim: hash binding must catch it.
    assert_eq!(
        sizes.original.max(sizes.redacted),
        changed.original.max(changed.redacted)
    );
    assert!(
        proof
            .verify_measured_with_policy(
                &current, &prepared, &target, &profile, &limits, "next:2", policy, changed,
            )
            .is_err()
    );
    let legacy = current
        .certify_unread_source_v9_with_policy(
            &prepared, &target, &profile, &limits, "next:2", policy,
        )
        .unwrap();
    assert_ne!(legacy.basis_hash(), proof.basis_hash());
    assert!(
        proof.continuation().choice.original_payload_bytes
            < legacy.continuation().choice.original_payload_bytes
    );
}

#[test]
fn analyst24_finish_decision_combines_replays_without_rewriting_historical_roles() {
    let policy = &INVESTIGATION_V2_POLICY;
    for (role, expected_schema) in [
        (SpecialistId::DomainAnalystV20, 3),
        (SpecialistId::DomainAnalystV21, 3),
        (SpecialistId::DomainAnalystV22, 4),
        (SpecialistId::DomainAnalystV23, 4),
        (SpecialistId::DomainAnalystV24, 6),
    ] {
        let (f, raw, _, _) = source_fixture(role, "bounded source");
        let input = source_choices(
            &f,
            build_source(&f, &raw, &[], role, CompletionCallMode::Choice),
            &[],
            &f.ledger,
        );
        let disclosure = input
            .seal_disclosure_with_policy(&prepare(&input), policy)
            .unwrap()
            .unwrap();
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: f.metadata.request_hash.clone(),
            obligations_hash: f.metadata.obligations_hash.clone(),
            obligations: f.request.obligations.clone(),
        };
        let bound = input
            .acquisition_choices
            .as_ref()
            .unwrap()
            .bind_completion_disclosure(
                "source-preview-task",
                "current:1",
                &contract,
                &f.metadata,
                input.ledger(),
                &disclosure,
                None,
                policy,
            )
            .unwrap();
        let replay = input
            .ledger()
            .coverage
            .as_ref()
            .unwrap()
            .qualification
            .coverage_id
            .clone();
        let finish = staged_finish::StagedFinish {
            schema_version: 1,
            outcome: staged_finish::CompletionOutcome::InsufficientEvidence,
            narrative: None,
            action: InvestigationAction::Finish {
                findings: vec![],
                knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
                limitations: vec![replay.clone(), replay, "not_available".into()],
            },
        };
        let decision = staged_finish::admit_finish_with_policy_using_consistency(
            &f.request,
            "source-preview-task",
            "current:1",
            &input,
            &disclosure,
            &finish,
            &[],
            None,
            "2026-11-06T00:00:00Z".into(),
            policy,
            Some(&bound),
        )
        .unwrap();
        let staged_finish::FinishDecision::Refused { feedback, .. } = decision else {
            panic!()
        };
        assert_eq!(feedback.schema_version, expected_schema);
        if role == SpecialistId::DomainAnalystV24 {
            let Some(staged_finish::ProseCorrection::Combined(combined)) =
                feedback.prose_correction
            else {
                panic!()
            };
            for limitation in [0, 1] {
                assert!(combined.entries.iter().any(|entry| matches!(entry,
                    staged_finish::CombinedCorrection::ReplayAt(location)
                    if location.start.field == replay_location::ProseField::Limitation { limitation })));
            }
            assert!(
                combined
                    .entries
                    .contains(&staged_finish::CombinedCorrection::RewriteSchemaPlaceholders)
            );
            assert_eq!(combined.omitted, 0);
        }
    }
}

#[path = "frozen_admission_replay_tests.rs"]
mod frozen_replay;

#[path = "historical_analyst_goldens_tests.rs"]
mod historical_goldens;

// AC-0566: the authenticated exact role gates new whole-field refusals.
#[test]
fn analyst26_bare_finish_fields_are_versioned_and_position_complete() {
    use staged_finish::{
        CombinedCorrection, CompletionOutcome, ProseCorrection, RefusalFeedback, StagedFinish,
    };
    for role in [
        SpecialistId::DomainAnalystV25,
        SpecialistId::DomainAnalystV26,
        SpecialistId::DomainAnalystV27,
    ] {
        let (f, raw, _, _) = source_fixture(role, "bounded source");
        let input = source_choices(
            &f,
            build_source(&f, &raw, &[], role, CompletionCallMode::Choice),
            &[],
            &f.ledger,
        );
        for text in [
            "statement",
            " Title ",
            "FINDINGS",
            "type",
            "finish",
            "Limitations",
            "limitation",
            "The statement is supported.",
        ] {
            for position in 0..4 {
                let mut value = StagedFinish {
                    schema_version: 1,
                    outcome: CompletionOutcome::Supported,
                    narrative: None,
                    action: InvestigationAction::Finish {
                        findings: vec![ProposedInvestigationFinding {
                            claim_kind: InvestigationClaimKind::ImplementedBehavior,
                            title: "Observed behavior".into(),
                            statement: "The selected behavior is bounded.".into(),
                            citation_ids: vec!["fact-1".into()],
                            limitations: vec!["Other behavior was not assessed.".into()],
                        }],
                        knowledge_completeness: KnowledgeCompleteness::Partial,
                        limitations: vec!["Other behavior was not assessed.".into()],
                    },
                };
                let InvestigationAction::Finish {
                    findings,
                    limitations,
                    ..
                } = &mut value.action
                else {
                    unreachable!()
                };
                match position {
                    0 => findings[0].title = text.into(),
                    1 => findings[0].statement = text.into(),
                    2 => findings[0].limitations = vec![text.into()],
                    _ => *limitations = vec![text.into()],
                }
                let feedback = RefusalFeedback::collect_input_corrections_with_policy(
                    &input,
                    &value,
                    None,
                    vec![],
                    &INVESTIGATION_V2_POLICY,
                )
                .unwrap();
                let has_new = feedback
                    .as_ref()
                    .and_then(|v| v.prose_correction.as_ref())
                    .is_some_and(|v| {
                        matches!(v, ProseCorrection::Combined(c)
                        if c.entries.contains(&CombinedCorrection::RewriteSchemaPlaceholders))
                    });
                let expected = matches!(
                    role,
                    SpecialistId::DomainAnalystV26 | SpecialistId::DomainAnalystV27
                ) && ["title", "statement", "findings", "type", "finish"]
                    .iter()
                    .any(|word| text.trim().eq_ignore_ascii_case(word));
                assert_eq!(has_new, expected, "{role:?} {text} position {position}");
            }
        }
    }
}

// AC-1444: with its only source read excluded for capacity, an @31 finish that
// does not name the gap is refused for it; the same @32 finish is accepted
// with a host-written limitation and the obligation reported as disclosed.
#[test]
fn analyst32_accepts_a_finish_whose_capacity_gap_the_host_discloses() {
    let policy = &INVESTIGATION_V2_POLICY;
    for role in [
        SpecialistId::DomainAnalystV31,
        SpecialistId::DomainAnalystV32,
    ] {
        let (f, raw, _, _) = source_fixture(role, "bounded source");
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: f.metadata.request_hash.clone(),
            obligations_hash: f.metadata.obligations_hash.clone(),
            obligations: f.request.obligations.clone(),
        };
        let offered = derive_source_choices(&f, &[], &f.ledger);
        let refused = offered
            .entries()
            .iter()
            .map(|entry| {
                let value = serde_json::to_value(&entry.action).unwrap();
                core_prov::content_hash(&bounded_bytes(&value, MAX_ACTION_BYTES).unwrap())
            })
            .collect();
        let exhausted = offered
            .with_capacity_obligations(&contract, &f.metadata, &f.ledger, &refused, policy)
            .unwrap();
        assert!(exhausted.capacity_exhausted());
        let gap = exhausted.capacity_excluded_obligations().to_vec();
        assert_eq!(gap.len(), 1);
        let input = build_source(&f, &raw, &[], role, CompletionCallMode::Choice)
            .with_acquisition_choices(std::sync::Arc::new(exhausted))
            .unwrap();
        let disclosure = input
            .seal_disclosure_with_policy(&prepare(&input), policy)
            .unwrap()
            .unwrap();
        let bound = input
            .acquisition_choices
            .as_ref()
            .unwrap()
            .bind_completion_disclosure(
                "source-preview-task",
                "current:1",
                &contract,
                &f.metadata,
                input.ledger(),
                &disclosure,
                None,
                policy,
            )
            .unwrap();
        let citation = input.ledger().citations[0].citation_id.clone();
        let finish = staged_finish::StagedFinish {
            schema_version: 1,
            outcome: staged_finish::CompletionOutcome::Supported,
            narrative: None,
            action: InvestigationAction::Finish {
                findings: vec![ProposedInvestigationFinding {
                    claim_kind: InvestigationClaimKind::InferredInterpretation,
                    title: "Candidate interpretation".into(),
                    statement: "A candidate interpretation requires a source check.".into(),
                    citation_ids: vec![citation],
                    limitations: vec!["Only selected material was considered.".into()],
                }],
                knowledge_completeness: KnowledgeCompleteness::Partial,
                limitations: vec!["Only selected material was considered.".into()],
            },
        };
        let decision = bound
            .admit_finish(
                &input,
                &f.request,
                &finish,
                &[],
                None,
                "2026-11-24T00:00:00Z".into(),
            )
            .unwrap();
        if role == SpecialistId::DomainAnalystV31 {
            let Err(staged_finish::FinishDecision::Refused { feedback, .. }) = decision else {
                panic!("@31 must refuse an unnamed capacity gap")
            };
            assert!(feedback.prose_correction.is_none());
            assert_eq!(feedback.unmet.len(), 1);
            assert_eq!(
                feedback.unmet[0].reason,
                staged_finish::RefusalReason::ExcludedByCapacity
            );
            continue;
        }
        let result = decision
            .ok()
            .expect("@32 accepts; the host discloses the gap");
        let name = staged_finish::capacity_name(&gap[0]);
        assert_eq!(
            result.limitations,
            vec![
                "Only selected material was considered.".to_string(),
                format!("{name}: {}", staged_finish::HOST_CAPACITY_DISCLOSURE),
            ]
        );
        let completion = result.completion.as_ref().unwrap();
        assert_eq!(
            completion.outcome,
            staged_finish::CompletionOutcome::Supported
        );
        let observed = completion
            .assessment
            .iter()
            .find(|item| item.obligation_id == gap[0])
            .unwrap();
        assert!(observed.capacity_disclosed && observed.unmet.is_none() && !observed.acquired);
    }
}

// AC-1443: a read refused for capacity leaves its obligation unattempted, so
// attempts never complete. The forced finish still gets the finish allowance.
#[test]
fn capacity_exhausted_finish_gets_the_local_finish_allowance() {
    for opted in [false, true] {
        let policy = if opted {
            INVESTIGATION_V2_POLICY
                .with_local_finish_generation()
                .unwrap()
        } else {
            INVESTIGATION_V2_POLICY.clone()
        };
        let role = SpecialistId::DomainAnalystV26;
        let (f, raw, _, _) = source_fixture(role, "bounded source");
        let input = InvestigationInput::prepare_v9_with_policy(
            &f.request.question,
            role,
            f.ledger.clone(),
            &[&raw],
            &[],
            &[],
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
        let offered = derive_source_choices(&f, &[], &f.ledger);
        let open = source_choices(&f, input.clone(), &[], &f.ledger);
        assert_eq!(open.invocation_output_tokens(&policy).unwrap(), 2048);
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: f.metadata.request_hash.clone(),
            obligations_hash: f.metadata.obligations_hash.clone(),
            obligations: f.request.obligations.clone(),
        };
        let refused = offered
            .entries()
            .iter()
            .map(|entry| {
                let value = serde_json::to_value(&entry.action).unwrap();
                core_prov::content_hash(&bounded_bytes(&value, MAX_ACTION_BYTES).unwrap())
            })
            .collect();
        let exhausted = offered
            .excluding_capacity_refusals(
                &contract,
                &f.metadata,
                &f.ledger,
                &refused,
                &INVESTIGATION_V2_POLICY,
            )
            .unwrap();
        assert!(exhausted.capacity_exhausted());
        assert!(!exhausted.attempts_complete());
        let input = input
            .with_acquisition_choices(std::sync::Arc::new(exhausted))
            .unwrap();
        assert_eq!(
            input.invocation_output_tokens(&policy).unwrap(),
            if opted { 4096 } else { 2048 }
        );
    }
}

#[test]
fn local_finish_policy_authenticates_complete_acquisition_and_restricts_schema() {
    use staged_finish::{FeedbackEntry, RefusalFeedback, RefusalReason};
    // AC-0287: the same completed attempt stays Choice under V2, but only
    // the opt-in lane becomes FinishOnly with a 4096-token allowance.
    for opted in [false, true] {
        for (complete, correction) in [(false, false), (true, false), (true, true)] {
            let policy = if opted {
                INVESTIGATION_V2_POLICY
                    .with_local_finish_generation()
                    .unwrap()
            } else {
                INVESTIGATION_V2_POLICY.clone()
            };
            let role = SpecialistId::DomainAnalystV26;
            let (mut f, raw, target, _) = source_fixture(role, "bounded source");
            let records = if complete {
                vec![source_record(
                    &f,
                    &target,
                    "step-1",
                    staged_attempt::AttemptOutcome::Unavailable {
                        reason: staged_attempt::UnavailableReason::PinnedTargetAbsent,
                    },
                )]
            } else {
                vec![]
            };
            f.metadata.completed_attempt_ids =
                records.iter().map(|r| r.attempt_id.clone()).collect();
            if correction {
                f.metadata.refusal_count = 1;
                f.metadata.last_refusal_id = Some("c".repeat(64));
                f.metadata.feedback = Some(RefusalFeedback {
                    citation_drop: None,
                    schema_version: 1,
                    prose_correction: None,
                    citation_correction: None,
                    unmet: vec![FeedbackEntry {
                        obligation_id: f.request.obligations.obligations[0].key().1.into(),
                        kind: "source".into(),
                        reason: RefusalReason::NotAttempted,
                    }],
                });
            }
            let input = InvestigationInput::prepare_v9_with_policy(
                &f.request.question,
                role,
                f.ledger.clone(),
                &[&raw],
                &[],
                &[],
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
            let input = source_choices(&f, input, &records, &f.ledger);
            let expected = if opted && complete { 4096 } else { 2048 };
            assert_eq!(input.invocation_output_tokens(&policy).unwrap(), expected);
            let mut values = CompletionLimits::default().values();
            values.max_output_tokens = expected;
            let limits = CompletionLimits::for_investigation(values).unwrap();
            let prepared = llm::EgressFirewall::new(llm::EgressPolicy::default())
                .prepare_bounded(
                    &DescriptorOnly,
                    &input.action("current:1".into()).unwrap(),
                    &limits,
                )
                .unwrap();
            let disclosure = input
                .seal_disclosure_with_policy(&prepared, &policy)
                .unwrap()
                .unwrap();
            let state = disclosure.admission.unwrap();
            assert_eq!(
                state.acquisition.as_ref().unwrap().finish_policy_complete,
                opted && complete
            );
            assert_eq!(
                state.mode == CompletionCallMode::FinishOnly,
                opted && complete
            );
            if opted && complete {
                let schema = input.payload.response_schema.as_ref().unwrap();
                assert!(!schema.contains("query_context"));
                assert!(!schema.contains("replay_query"));
                assert!(
                    !input.permits_acquisition(&QualifiedInvestigationAction::SourceOrFinish(
                        InvestigationAction::ReadEvidence {
                            fact: target.fact.clone(),
                            role: target.role,
                            index: target.index,
                        },
                    ))
                );
                let mut changed_mode = state.clone();
                changed_mode.mode = CompletionCallMode::Choice;
                assert!(
                    changed_mode
                        .validate_mode_with_metadata(&f.metadata, &policy)
                        .is_err()
                );
                assert!(
                    state
                        .validate_saved_role_shape(role, &INVESTIGATION_V2_POLICY)
                        .is_err()
                );
            }
        }
    }
}

#[path = "input_bounded_tests.rs"]
mod bounded_tests;

#[path = "visible_schema_tests.rs"]
mod visible_schema_tests;

// AC-1413 / #679: a firewall-refused acquired read stays in the ledger, is
// withheld from the payload, and seals as a distinct `firewall_refused` source.
#[test]
fn withheld_refused_read_reaches_no_payload_and_seals_distinctly() {
    let policy = &INVESTIGATION_V2_POLICY;
    let role = SpecialistId::DomainAnalystV10;
    let text = "refused-private-text-7f3a";
    let (mut fixture, raw, target, prototype) = source_fixture(role, text);
    let mut evidence = vec![];
    acquire_source(&mut fixture, &target, &prototype, &mut evidence, text);
    let id = evidence[0].id.clone();
    let prepare_with = |evidence: &[AgentEvidence], withheld: &BTreeSet<String>| {
        InvestigationInput::prepare_v9_withholding_with_policy(
            &fixture.request.question,
            role,
            fixture.ledger.clone(),
            &[&raw],
            evidence,
            &[],
            &usage(CompletionCallMode::Choice),
            &fixture.basis,
            &fixture.request,
            &fixture.metadata,
            &[],
            CompletionCallMode::Choice,
            0,
            None,
            policy,
            withheld,
        )
    };
    let withheld = BTreeSet::from([id.clone()]);
    let input = prepare_with(&[], &withheld).unwrap();
    let payload = &input.action("current:1".into()).unwrap().payload;
    assert!(payload.spans.is_empty());
    assert!(!payload.prompt.contains(text));
    let prompt: Value = serde_json::from_str(&payload.prompt).unwrap();
    assert_eq!(prompt["withheld_sources"].as_array().unwrap().len(), 1);
    assert_eq!(prompt["withheld_sources"][0]["outcome"], "firewall_refused");
    assert_eq!(prompt["withheld_sources"][0]["handle"]["citation_id"], id);
    assert!(
        prompt["citations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|handle| handle["citation_id"] != id)
    );
    let disclosure = input
        .seal_disclosure_with_policy(&prepare(&input), policy)
        .unwrap()
        .unwrap();
    let source = disclosure
        .sources
        .iter()
        .find(|source| source.citation_id == id)
        .unwrap();
    assert_eq!(
        source.representation,
        DisclosureRepresentation::FirewallRefused
    );
    assert!(source.disclosed_text_hash.is_none() && !source.redacted);
    // Citation schemas never offer the withheld id, while the ledger keeps it.
    assert!(input.ledger().citations.iter().any(|c| c.citation_id == id));
    assert!(
        input
            .citable_ledger()
            .unwrap()
            .citations
            .iter()
            .all(|c| c.citation_id != id)
    );
    // A firewall refusal is a source state only, never a history state.
    let mut forged = disclosure.clone();
    forged.history = Some(DisclosureRepresentation::FirewallRefused);
    assert!(forged.validate_with_policy(input.ledger(), policy).is_err());
    // A later read can still be measured on the withholding input.
    input
        .measure_unread_source_v9_with_policy(
            &target,
            &profile(),
            &CompletionLimits::default(),
            "next:2",
            policy,
        )
        .unwrap();

    // Only an acquired source citation without supplied text can be withheld.
    assert!(prepare_with(&[], &BTreeSet::new()).is_err());
    assert!(prepare_with(&evidence, &withheld).is_err());
    assert!(prepare_with(&[], &BTreeSet::from(["evidence-9".to_string()])).is_err());

    // No withholding keeps the legacy prompt identity.
    let legacy = build_source(&fixture, &raw, &evidence, role, CompletionCallMode::Choice);
    let empty = prepare_with(&evidence, &BTreeSet::new()).unwrap();
    let legacy_payload = legacy.action("current:1".into()).unwrap().payload;
    assert_eq!(
        legacy_payload,
        empty.action("current:1".into()).unwrap().payload
    );
    assert!(!legacy_payload.prompt.contains("withheld_sources"));
}

#[test]
fn withheld_sources_parser_accepts_only_closed_unique_entries() {
    let entry = |id: &str| json!({"handle": {"citation_id": id}, "outcome": "firewall_refused"});
    assert!(withheld_source_ids(&json!({})).unwrap().is_empty());
    assert_eq!(
        withheld_source_ids(&json!({"withheld_sources": [entry("a"), entry("b")]})).unwrap(),
        BTreeSet::from(["a", "b"])
    );
    for bad in [
        json!({"withheld_sources": []}),
        json!({"withheld_sources": [entry("a"), entry("a")]}),
        json!({"withheld_sources": [{"handle": {"citation_id": "a"}, "outcome": "redacted"}]}),
        json!({"withheld_sources": [{"handle": {"citation_id": "a"}, "outcome": "firewall_refused", "text": "x"}]}),
        json!({"withheld_sources": [{"outcome": "firewall_refused", "handle": {}}]}),
    ] {
        assert!(withheld_source_ids(&bad).is_err(), "{bad}");
    }
}

// AC-1435 (#2478): @31 turns every non-offered citation into one bounded
// drop-only correction in choice and finish-only modes; an offered target in
// choice mode keeps the typed acquisition correction; @30 is unchanged.
#[test]
fn analyst31_drops_non_offered_citations_under_the_single_repair() {
    let policy = &INVESTIGATION_V2_POLICY;
    for role in [
        SpecialistId::DomainAnalystV30,
        SpecialistId::DomainAnalystV31,
    ] {
        for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
            let (mut f, raw, target, _) = source_fixture(role, "bounded source");
            f.request.specialist_id = serde_json::to_value(role).unwrap().as_str().unwrap().into();
            let input = source_choices(&f, build_source(&f, &raw, &[], role, mode), &[], &f.ledger);
            let disclosure = input
                .seal_disclosure_with_policy(&prepare(&input), policy)
                .unwrap()
                .unwrap();
            let contract = InvestigationCompletionContract {
                schema_version: 1,
                request_hash: f.metadata.request_hash.clone(),
                obligations_hash: f.metadata.obligations_hash.clone(),
                obligations: f.request.obligations.clone(),
            };
            let bound = input
                .acquisition_choices
                .as_ref()
                .unwrap()
                .bind_completion_disclosure(
                    "source-preview-task",
                    "current:1",
                    &contract,
                    &f.metadata,
                    input.ledger(),
                    &disclosure,
                    None,
                    policy,
                )
                .unwrap();
            // (case, cited ids): A offered target, B offered plus hallucinated,
            // C a graph-fact id, D an id that is not a plain identifier.
            let cases: [(&str, Vec<String>); 4] = [
                ("offered", vec![target.target_id.clone()]),
                (
                    "mixed",
                    vec![target.target_id.clone(), "evidence-99".into()],
                ),
                ("graph-fact", vec!["sym:src/a.ts#Admin".into()]),
                ("authored", vec!["not an id".into()]),
            ];
            for (case, ids) in cases {
                let finish = staged_finish::StagedFinish {
                    schema_version: 1,
                    outcome: staged_finish::CompletionOutcome::Supported,
                    narrative: None,
                    action: InvestigationAction::Finish {
                        findings: vec![ProposedInvestigationFinding {
                            claim_kind: InvestigationClaimKind::InferredInterpretation,
                            title: "Candidate interpretation".into(),
                            statement: "A candidate interpretation requires a source check.".into(),
                            citation_ids: ids.clone(),
                            limitations: vec!["Only selected material was considered.".into()],
                        }],
                        knowledge_completeness: KnowledgeCompleteness::Partial,
                        limitations: vec!["Only selected material was considered.".into()],
                    },
                };
                let result = staged_finish::admit_finish_with_policy_using_consistency(
                    &f.request,
                    "source-preview-task",
                    "current:1",
                    &input,
                    &disclosure,
                    &finish,
                    &[],
                    None,
                    "2026-11-03T00:00:00Z".into(),
                    policy,
                    Some(&bound),
                );
                let label = format!("{role:?} {mode:?} {case}");
                let typed = role == SpecialistId::DomainAnalystV30
                    || (case == "offered" && mode == CompletionCallMode::Choice);
                if role == SpecialistId::DomainAnalystV30
                    && !(case == "offered" && mode == CompletionCallMode::Choice)
                    || case == "authored"
                {
                    assert_eq!(
                        result.err(),
                        Some(InvestigationError::InvalidCitation),
                        "{label}"
                    );
                    continue;
                }
                let staged_finish::FinishDecision::Refused {
                    assessment,
                    feedback,
                } = result.unwrap()
                else {
                    panic!("{label}: a citation correction never yields a result");
                };
                if typed {
                    assert_eq!(feedback.schema_version, 2, "{label}");
                    assert!(feedback.citation_correction.is_some(), "{label}");
                    assert!(feedback.citation_drop.is_none(), "{label}");
                    continue;
                }
                let mut expected = ids.clone();
                expected.sort();
                assert_eq!(feedback.schema_version, 7, "{label}");
                assert!(feedback.citation_correction.is_none(), "{label}");
                assert!(feedback.prose_correction.is_none(), "{label}");
                assert_eq!(
                    feedback.citation_drop,
                    Some(staged_finish::CitationDrop {
                        citation_ids: expected
                    }),
                    "{label}"
                );
                // The drop never offers acquisition or discharges anything.
                assert!(!feedback.unmet.is_empty(), "{label}");
                let wire = serde_json::to_value(&feedback).unwrap();
                assert!(wire.get("citation_correction").is_none(), "{label}");
                let mut refusal = staged_refusal::RefusalRecord {
                    schema_version: 1,
                    refusal_id: String::new(),
                    task_id: "source-preview-task".into(),
                    request_hash: f.metadata.request_hash.clone(),
                    obligations_hash: f.metadata.obligations_hash.clone(),
                    step_id: "current:1".into(),
                    input_ledger_hash: f.ledger.fingerprint_with_policy(policy).unwrap(),
                    disclosure_hash: bound.fingerprint().unwrap(),
                    finish_hash: finish.fingerprint().unwrap(),
                    outcome: finish.outcome,
                    response_hash: core_prov::content_hash(b"synthetic drop response"),
                    previous_refusal: None,
                    refusal_count: 1,
                    assessment: assessment.clone(),
                    feedback: feedback.clone(),
                };
                refusal.refusal_id = refusal.content_id().unwrap();
                // First refusal continues; the shared allowance makes a second
                // refusal terminal, exactly as for every other correction.
                assert_eq!(
                    bound
                        .validate_refusal(
                            &refusal,
                            &f.metadata.request_hash,
                            &f.request.obligations,
                            &[],
                            None
                        )
                        .unwrap(),
                    staged_refusal::RefusalDisposition::Continue,
                    "{label}"
                );
                let mut saved = f.metadata.clone();
                saved.refusal_count = 1;
                saved.last_refusal_id = Some(refusal.refusal_id.clone());
                saved.feedback = Some(feedback.clone());
                assert_eq!(
                    saved
                        .finish_call_reserve_with_policy(policy)
                        .unwrap()
                        .model_invocations,
                    1,
                    "{label}: one corrected finish remains, never a second repair"
                );
                // Without the invocation witness a stored drop is not accepted.
                assert!(
                    refusal
                        .validate_stored_basis_with_policy(
                            &contract.request_hash,
                            &contract.obligations,
                            input.ledger(),
                            &disclosure,
                            None,
                            policy
                        )
                        .is_err(),
                    "{label}"
                );
            }
        }
    }
}

// AC-1435: the v7 shape is exact: sorted, unique, plain, bounded ids, and
// never alongside a typed or prose correction.
#[test]
fn citation_drop_feedback_shape_is_exact() {
    let policy = &INVESTIGATION_V2_POLICY;
    let entry = staged_finish::FeedbackEntry {
        obligation_id: "a".repeat(64),
        kind: "source".into(),
        reason: staged_finish::RefusalReason::NotAttempted,
    };
    let drop = |ids: &[&str]| staged_finish::RefusalFeedback {
        schema_version: 7,
        prose_correction: None,
        citation_correction: None,
        citation_drop: Some(staged_finish::CitationDrop {
            citation_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
        }),
        unmet: vec![],
    };
    assert!(
        drop(&["evidence-9", "sym:a.ts#B"])
            .bytes_with_policy(policy)
            .is_ok()
    );
    // A hallucinated id with every obligation met still refuses.
    assert!(drop(&["evidence-9"]).unmet.is_empty());
    for bad in [
        drop(&[]),
        drop(&["b", "a"]),
        drop(&["a", "a"]),
        drop(&["has space"]),
        drop(&[&"x".repeat(129)]),
    ] {
        assert!(bad.bytes_with_policy(policy).is_err(), "{bad:?}");
    }
    let many = (0..=staged_finish::MAX_CITATION_DROP_IDS)
        .map(|n| format!("e{n:03}"))
        .collect::<Vec<_>>();
    let refs = many.iter().map(String::as_str).collect::<Vec<_>>();
    assert!(drop(&refs).bytes_with_policy(policy).is_err());
    let mut wrong_version = drop(&["a"]);
    wrong_version.schema_version = 2;
    assert!(wrong_version.bytes_with_policy(policy).is_err());
    let mut with_prose = drop(&["a"]);
    with_prose.unmet = vec![entry];
    with_prose.prose_correction =
        Some(staged_finish::ProseCorrection::RewriteSchemaPlaceholdersAsPlainProse);
    assert!(with_prose.bytes_with_policy(policy).is_err());
    // Historical feedback never carries the field.
    let historical = serde_json::to_value(staged_finish::RefusalFeedback {
        citation_drop: None,
        ..drop(&["a"])
    })
    .unwrap();
    assert!(historical.get("citation_drop").is_none());
}
