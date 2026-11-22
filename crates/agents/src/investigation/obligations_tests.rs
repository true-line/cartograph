use super::*;
use serde_json::json;

fn source() -> Obligation {
    let target = super::super::tests::inspection_tests::fixture().0;
    identified(Obligation::Source {
        obligation_id: String::new(),
        target: Box::new(target),
    })
}
fn inventory(context: char) -> Obligation {
    identified(Obligation::Inventory {
        obligation_id: String::new(),
        target: InventoryTarget {
            context_id: format!("context-v2:{}", context.to_string().repeat(64)),
            repo: "example/app".into(),
            inventory_hash: "a".repeat(64),
        },
    })
}
fn identified(mut value: Obligation) -> Obligation {
    let id = value.content_id().unwrap();
    match &mut value {
        Obligation::Source { obligation_id, .. } | Obligation::Inventory { obligation_id, .. } => {
            *obligation_id = id
        }
    }
    value
}
fn envelope(values: Vec<Obligation>) -> ObligationEnvelope {
    ObligationEnvelope {
        schema_version: 1,
        contract: CONTRACT.into(),
        obligations: values,
    }
}
fn request() -> StagedObligationRequest {
    let target = source();
    let snapshot = match &target {
        Obligation::Source { target, .. } => target.graph_snapshot_id.clone(),
        _ => unreachable!(),
    };
    StagedObligationRequest {
        schema_version: 4,
        request_nonce: "request-1".into(),
        specialist_id: "domain-analyst@7".into(),
        obligations: envelope(vec![target, inventory('b')]),
        inspection_requirements: None,
        question: "Inspect selected evidence".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: "investigation-v1".into(),
        expected_graph_revision: Some(snapshot),
        expected_context_id: format!("context-v2:{}", "b".repeat(64)),
        conversation_id: None,
        parent_id: None,
    }
}

// Historical normalization oracle, kept only in tests. Production admission
// borrows common fields and never creates this artificial legacy request.
fn historical_projection(value: &StagedObligationRequest) -> StartInvestigationRequest {
    StartInvestigationRequest {
        schema_version: 3,
        request_nonce: value.request_nonce.clone(),
        specialist_id: SpecialistId::DomainAnalystV6,
        inspection_requirements: None,
        question: value.question.clone(),
        scope: value.scope.clone(),
        provider_mode: value.provider_mode,
        limit_profile: value.limit_profile.clone(),
        expected_graph_revision: value.expected_graph_revision.clone(),
        expected_context_id: Some(value.expected_context_id.clone()),
        conversation_id: value.conversation_id.clone(),
        parent_id: value.parent_id.clone(),
    }
}

#[test]
fn staged_obligations_common_field_validation_matches_legacy() {
    // AC-0264: extracting borrowed validation preserves every common rejection.
    for case in [
        "empty_question",
        "long_question",
        "nul_question",
        "empty_nonce",
        "long_nonce",
        "invalid_nonce",
        "unknown_profile",
        "invalid_graph",
        "invalid_context",
        "empty_context",
        "invalid_conversation",
        "long_parent",
        "orphan_parent",
        "empty_anchor",
        "long_anchor",
        "zero_hops",
        "excess_hops",
    ] {
        let mut value = request();
        value.obligations = envelope(vec![inventory('b')]);
        match case {
            "empty_question" => value.question = " ".into(),
            "long_question" => value.question = "Q".repeat(MAX_QUESTION_BYTES + 1),
            "nul_question" => value.question.push('\0'),
            "empty_nonce" => value.request_nonce.clear(),
            "long_nonce" => value.request_nonce = "n".repeat(129),
            "invalid_nonce" => value.request_nonce.push('/'),
            "unknown_profile" => value.limit_profile = "investigation-v2".into(),
            "invalid_graph" => value.expected_graph_revision = Some("invalid".into()),
            "invalid_context" => value.expected_context_id = "context-v1:invalid".into(),
            "empty_context" => value.expected_context_id.clear(),
            "invalid_conversation" => value.conversation_id = Some("/invalid".into()),
            "long_parent" => {
                value.conversation_id = Some("conversation".into());
                value.parent_id = Some("p".repeat(129));
            }
            "orphan_parent" => value.parent_id = Some("parent".into()),
            _ => {
                value.scope = InvestigationScope::Neighborhood {
                    anchor: match case {
                        "empty_anchor" => String::new(),
                        "long_anchor" => "a".repeat(1025),
                        _ => "anchor".into(),
                    },
                    hops: match case {
                        "zero_hops" => 0,
                        "excess_hops" => 4,
                        _ => 1,
                    },
                }
            }
        }
        assert_eq!(
            historical_projection(&value).validate(),
            Err(InvestigationError::InvalidRequest),
            "legacy {case}"
        );
        assert!(
            matches!(value.normalized(), Err(InvestigationError::InvalidRequest)),
            "staged {case}"
        );
    }
}

#[test]
fn staged_obligations_projection_bound_is_dominated_by_whole_request() {
    // AC-0264: every optional-field presence combination and both closed roles
    // retain identical common encodings; the historical projection is smaller.
    for role in ["domain-analyst@7", "evidence-auditor@7"] {
        for presence in 0..8 {
            let mut value = request();
            value.specialist_id = role.into();
            value.obligations = envelope(vec![inventory('b')]);
            if presence & 1 == 0 {
                value.expected_graph_revision = None;
            }
            if presence & 2 != 0 {
                value.conversation_id = Some("conversation".into());
            }
            if presence & 4 != 0 {
                value.parent_id = Some("parent".into());
            }
            let projected = historical_projection(&value);
            assert!(
                serde_json::to_vec(&projected).unwrap().len()
                    < serde_json::to_vec(&value).unwrap().len()
            );
            assert_eq!(projected.validate().is_ok(), value.normalized().is_ok());
            if value.normalized().is_ok() {
                let wire = serde_json::to_vec(&value).unwrap();
                assert_eq!(
                    wire,
                    serde_json::to_vec(&value.normalized().unwrap()).unwrap()
                );
            }
        }
    }
}

// AC-0264: raw bounds precede ownership, identity/order normalization is exact.
#[test]
fn staged_obligations_normalize_mixed_inventory_only_and_source_conflicts() {
    let first = source();
    let second = inventory('b');
    let a = envelope(vec![first.clone(), second.clone(), first.clone()]);
    let b = envelope(vec![second.clone(), first.clone()]);
    assert_eq!(a.fingerprint().unwrap(), b.fingerprint().unwrap());
    assert_eq!(a.normalized().unwrap().obligations.len(), 2);
    assert_eq!(a.sources().unwrap().len(), 1);
    assert!(envelope(vec![second.clone()]).sources().unwrap().is_empty());
    assert_eq!(
        envelope(vec![second, inventory('c')])
            .normalized()
            .unwrap()
            .obligations
            .len(),
        2
    );
    assert_eq!(
        envelope(vec![inventory('b'); 12])
            .normalized()
            .unwrap()
            .obligations
            .len(),
        1
    );
    let mut conflicting_inventory = inventory('b');
    if let Obligation::Inventory { target, .. } = &mut conflicting_inventory {
        target.inventory_hash = "c".repeat(64);
    }
    assert!(
        envelope(vec![inventory('b'), identified(conflicting_inventory)])
            .normalized()
            .is_err()
    );
    assert!(envelope(vec![]).normalized().is_err());
    assert!(envelope(vec![first.clone(); 13]).normalized().is_err());
    assert!(envelope(vec![first.clone(); 12]).normalized().is_err()); // descriptors exceed aggregate bytes even when duplicate
    let mut conflict = first.clone();
    if let Obligation::Source { target, .. } = &mut conflict {
        target.receipt_inventory_index += 1;
        target.target_id = target.content_id().unwrap();
    }
    assert!(
        envelope(vec![first.clone(), identified(conflict)])
            .normalized()
            .is_err()
    );
    let mut forged = first;
    if let Obligation::Source { obligation_id, .. } = &mut forged {
        *obligation_id = "0".repeat(64);
    }
    assert!(envelope(vec![forged]).normalized().is_err());
}

#[test]
fn staged_obligations_request_rejects_dual_authority_null_and_unknown_fields() {
    let valid = request();
    let raw = serde_json::to_string(&valid).unwrap();
    let parsed = StagedObligationRequest::decode(&raw).unwrap();
    assert_eq!(parsed.intent_hash().unwrap(), valid.intent_hash().unwrap());
    assert!(!raw.contains("inspection_requirements"));
    let value = serde_json::to_value(&valid).unwrap();
    for field in ["inspection_requirements", "obligations"] {
        let mut changed = value.clone();
        changed[field] = serde_json::Value::Null;
        assert!(StagedObligationRequest::decode(&changed.to_string()).is_err());
    }
    let mut both = valid.clone();
    both.inspection_requirements = Some(valid.obligations.sources().unwrap());
    assert!(both.normalized().is_err());
    let mut unknown = value.clone();
    unknown["obligations"]["obligations"][0]["unexpected"] = json!(true);
    assert!(StagedObligationRequest::decode(&unknown.to_string()).is_err());
    unknown = value;
    unknown["obligations"]["obligations"][0]["kind"] = json!("history");
    assert!(StagedObligationRequest::decode(&unknown.to_string()).is_err());
    assert!(StagedObligationRequest::decode(&format!("{}{}", raw, " ".repeat(8192))).is_err());
    assert!(
        StagedObligationRequest::decode(&raw.replacen(
            "\"schema_version\":4",
            "\"schema_version\":4,\"schema_version\":4",
            1
        ))
        .is_err()
    );
}

#[test]
fn staged_obligations_every_inventory_component_and_request_intent_are_bound() {
    let original = inventory('b');
    for field in ["context", "repo", "hash"] {
        let mut changed = original.clone();
        if let Obligation::Inventory { target, .. } = &mut changed {
            match field {
                "context" => target.context_id = format!("context-v2:{}", "c".repeat(64)),
                "repo" => target.repo = "example/other".into(),
                _ => target.inventory_hash = "c".repeat(64),
            }
        }
        assert_ne!(
            original.content_id().unwrap(),
            changed.content_id().unwrap()
        );
        assert_ne!(
            envelope(vec![original.clone()]).fingerprint().unwrap(),
            envelope(vec![identified(changed)]).fingerprint().unwrap()
        );
    }
    let original = request();
    let hash = original.intent_hash().unwrap();
    let mut changed = original.clone();
    changed.question.push('?');
    assert_ne!(hash, changed.intent_hash().unwrap());
    changed = original.clone();
    changed.request_nonce.push('2');
    assert_ne!(hash, changed.intent_hash().unwrap());
    changed = original.clone();
    changed.specialist_id = "evidence-auditor@7".into();
    assert_ne!(hash, changed.intent_hash().unwrap());
    changed = original;
    changed.expected_context_id = format!("context-v2:{}", "c".repeat(64));
    assert!(changed.normalized().is_err());
}

#[test]
fn staged_obligations_full_request_exact_byte_boundary_and_maximal_descriptors() {
    let mut value = request();
    // Whitespace is charged by strict decoding even when canonical request is shorter.
    let raw = serde_json::to_string(&value).unwrap();
    let padded = format!("{}{}", raw, " ".repeat(8192 - raw.len()));
    StagedObligationRequest::decode(&padded).unwrap();
    assert!(StagedObligationRequest::decode(&(padded + " ")).is_err());
    // Canonical complete request boundary, not just raw whitespace admission.
    let mut canonical = request();
    canonical.obligations = envelope(
        (0..12)
            .map(|index| {
                let mut item = inventory('b');
                if let Obligation::Inventory { target, .. } = &mut item {
                    target.repo = format!("{index:02}{}", "r".repeat(254));
                }
                identified(item)
            })
            .collect(),
    );
    canonical.question = "Q".into();
    let base_bytes = serde_json::to_vec(&canonical).unwrap().len();
    assert!(base_bytes < 8192 && 8192 - base_bytes < MAX_QUESTION_BYTES);
    canonical.question.push_str(&"x".repeat(8192 - base_bytes));
    assert_eq!(serde_json::to_vec(&canonical).unwrap().len(), 8192);
    historical_projection(&canonical).validate().unwrap();
    canonical.normalized().unwrap();
    canonical.question.push('x');
    assert!(matches!(
        canonical.normalized(),
        Err(InvestigationError::LimitExceeded)
    ));
    let mut maximal = source();
    if let Obligation::Source { target, .. } = &mut maximal {
        target.source.path = "x".repeat(1024);
        target.captured.file.path = target.source.path.clone();
        target.source.repo = "r".repeat(256);
        target.source.commit_sha = "c".repeat(256);
        target.target_id = target.content_id().unwrap();
    }
    value.question = "Q".into();
    value.obligations = envelope(vec![identified(maximal); 12]);
    assert!(matches!(
        value.normalized(),
        Err(InvestigationError::LimitExceeded)
    ));
}

#[test]
fn staged_obligations_source_identity_and_pinned_request_basis_are_bound() {
    let original = source();
    let original_id = original.content_id().unwrap();
    for field in [
        "graph",
        "digest",
        "receipt",
        "occurrence",
        "range",
        "capture",
        "path",
    ] {
        let mut changed = original.clone();
        if let Obligation::Source { target, .. } = &mut changed {
            match field {
                "graph" => target.graph_snapshot_id = format!("context-v1:{}", "c".repeat(64)),
                "digest" => target.fact_digest = format!("node-v1:{}", "c".repeat(64)),
                "receipt" => target.receipt_id = format!("ts-primary-v2:{}", "a".repeat(64)),
                "occurrence" => target.receipt_inventory_index += 1,
                "range" => {
                    target.source.byte_end -= 1;
                    target.captured.byte_end -= 1;
                }
                "capture" => {
                    target.captured.file.capture_id = format!("capture-v1:{}", "a".repeat(64))
                }
                _ => {
                    target.source.path = "other.ts".into();
                    target.captured.file.path = "other.ts".into();
                }
            }
            target.target_id = target.content_id().unwrap();
        }
        assert_ne!(original_id, changed.content_id().unwrap(), "{field}");
    }
    let mut value = request();
    value.expected_graph_revision = Some(format!("context-v1:{}", "a".repeat(64)));
    assert!(value.normalized().is_err());
    let mut value = request();
    value.specialist_id = "evidence-auditor@7".into();
    value.normalized().unwrap();
    value.specialist_id = "domain-analyst@6".into();
    assert!(value.normalized().is_err());
}

// AC-0264: these literal bytes/identities freeze the pre-staging public contracts.
#[test]
fn staged_obligations_legacy_literal_goldens() {
    let (target, _, _) = super::super::tests::inspection_tests::fixture();
    let mut legacy = super::super::tests::request();
    let mut requests = Vec::new();
    for generation in 1..=3 {
        legacy.schema_version = generation;
        legacy.specialist_id = match generation {
            1 => SpecialistId::DomainAnalyst,
            2 => SpecialistId::DomainAnalystV5,
            _ => SpecialistId::DomainAnalystV6,
        };
        if generation == 2 {
            legacy.inspection_requirements = Some(vec![target.clone()]);
            legacy.expected_graph_revision = Some(target.graph_snapshot_id.clone());
        }
        if generation == 3 {
            legacy.inspection_requirements = None;
            legacy.expected_context_id = Some(format!("context-v2:{}", "b".repeat(64)));
        }
        requests.push(json!({"wire":serde_json::to_string(&legacy).unwrap(),"intent_hash":legacy.intent_hash().unwrap()}));
    }
    let definitions: Vec<_> = [
        SpecialistId::DomainAnalystV1,
        SpecialistId::EvidenceAuditorV1,
        SpecialistId::DomainAnalyst,
        SpecialistId::EvidenceAuditor,
        SpecialistId::DomainAnalystV3,
        SpecialistId::EvidenceAuditorV3,
        SpecialistId::DomainAnalystV4,
        SpecialistId::EvidenceAuditorV4,
        SpecialistId::DomainAnalystV5,
        SpecialistId::EvidenceAuditorV5,
        SpecialistId::DomainAnalystV6,
        SpecialistId::EvidenceAuditorV6,
    ]
    .into_iter()
    .map(|id| {
        let definition = id.definition().unwrap();
        json!({"id":definition.id,"prompt_fingerprint":definition.prompt_fingerprint})
    })
    .collect();
    let ledger = super::super::tests::ledger();
    // Use the existing valid compact input fixture for its sealed representation.
    let prepared_input = super::super::tests::sealing_tests::input(SpecialistId::DomainAnalystV3);
    let payload = prepared_input
        .action("golden:step-1".into())
        .unwrap()
        .payload;
    let (result_ledger, disclosure, action) = super::super::result_disclosure_tests::basis();
    let result = InvestigationResult::admit(
        "investigation-1",
        &result_ledger,
        &action,
        std::iter::empty(),
        None,
        "2026-10-19T00:00:00Z".into(),
    )
    .unwrap();
    let actual = json!({"requests":requests,"definitions":definitions,"input_payload_hash":core_prov::content_hash(&serde_json::to_vec(&payload).unwrap()),
        "ledger_hash":ledger.fingerprint().unwrap(),"disclosure_hash":disclosure.fingerprint(&result_ledger).unwrap(),"result_id":result.result_id,
        "result_wire":serde_json::to_string(&result).unwrap()});
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/legacy-obligation-goldens.json")).unwrap();
    assert_eq!(
        actual,
        expected,
        "LEGACY_GOLDEN={}",
        serde_json::to_string(&actual).unwrap()
    );
}

#[test]
fn opt_in_envelope_admits_raw_capacity_without_reinterpreting_v1() {
    // AC-0281: opt-in admission has a separate identity and preserves legacy bounds.
    let policy = &super::super::policy::INVESTIGATION_V2_POLICY;
    let legacy = envelope(vec![inventory('b')]);
    let legacy_id = legacy.fingerprint().unwrap();
    assert!(legacy.normalized_with_policy(policy).is_err());
    let mut opt_in = ObligationEnvelope {
        schema_version: 2,
        contract: "investigation-completion-obligations@2".into(),
        obligations: vec![inventory('b'); 56],
    };
    assert!(opt_in.normalized().is_err());
    let normalized = opt_in.normalized_with_policy(policy).unwrap();
    assert_eq!(normalized.obligations.len(), 1);
    assert_ne!(
        normalized.fingerprint_with_policy(policy).unwrap(),
        legacy_id
    );
    assert_eq!(
        normalized.obligations[0].content_id().unwrap(),
        legacy.obligations[0].content_id().unwrap()
    );
    opt_in.obligations.push(inventory('b'));
    assert!(opt_in.normalized_with_policy(policy).is_err());
    assert_eq!(
        InvestigationPolicy::for_profile(policy.profile())
            .unwrap()
            .limits(),
        policy.limits()
    );
    assert_eq!(
        InvestigationPolicy::for_limits(&policy.limits())
            .unwrap()
            .profile(),
        policy.profile()
    );
}

#[test]
fn opt_in_request_family_binds_policy_role_and_envelope() {
    // AC-0281: selecting a larger policy never reinterprets a legacy request.
    let policy = &super::super::policy::INVESTIGATION_V2_POLICY;
    let legacy = request();
    let old_hash = legacy.intent_hash().unwrap();
    assert!(legacy.normalized_with_policy(policy).is_err());
    let mut selected = legacy.clone();
    selected.schema_version = 5;
    selected.specialist_id = "domain-analyst@8".into();
    selected.limit_profile = policy.profile().into();
    selected.obligations.schema_version = 2;
    selected.obligations.contract = "investigation-completion-obligations@2".into();
    let raw = serde_json::to_string(&selected).unwrap();
    assert!(StagedObligationRequest::decode(&raw).is_err());
    let admitted = StagedObligationRequest::decode_with_policy(&raw, policy).unwrap();
    assert_ne!(admitted.intent_hash_with_policy(policy).unwrap(), old_hash);
    for field in [
        "schema_version",
        "specialist_id",
        "limit_profile",
        "obligations",
    ] {
        let mut mixed = serde_json::to_value(&selected).unwrap();
        mixed[field] = serde_json::to_value(&legacy).unwrap()[field].clone();
        assert!(
            StagedObligationRequest::decode_with_policy(&mixed.to_string(), policy).is_err(),
            "{field}"
        );
    }
    let padded = format!("{}{}", raw, " ".repeat(policy.request_bytes()));
    assert!(StagedObligationRequest::decode_with_policy(&padded, policy).is_err());
    assert_eq!(legacy.intent_hash().unwrap(), old_hash);
}

// AC-1429: every offered version that selects the expanded policy has a
// request family, so a start of it is not refused as invalid input.
#[test]
fn every_offered_policy_version_has_a_request_family() {
    let policy = &super::super::policy::INVESTIGATION_V2_POLICY;
    for id in offered_specialists()
        .into_iter()
        .filter(|id| id.uses_policy_selection() && id.uses_next_prompt_admission())
    {
        let wire = serde_json::to_value(id).unwrap();
        let wire = wire.as_str().unwrap();
        assert!(
            (1..=8).any(|schema| request_family(policy, schema, wire).is_ok()),
            "{wire}"
        );
    }
    assert_eq!(
        request_family(policy, 7, "domain-analyst@29").unwrap().1,
        &["domain-analyst@29"]
    );
}
