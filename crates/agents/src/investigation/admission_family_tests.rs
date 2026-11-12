use super::*;

fn request(role: &str) -> StagedObligationRequest {
    let context_id = format!("context-v2:{}", "b".repeat(64));
    let mut item = Obligation::Inventory {
        obligation_id: String::new(),
        target: InventoryTarget {
            context_id: context_id.clone(),
            repo: "example/app".into(),
            inventory_hash: "a".repeat(64),
        },
    };
    let id = item.content_id().unwrap();
    if let Obligation::Inventory { obligation_id, .. } = &mut item {
        *obligation_id = id;
    }
    StagedObligationRequest {
        schema_version: 7,
        request_nonce: "admission-family-fixture".into(),
        specialist_id: role.into(),
        obligations: ObligationEnvelope {
            schema_version: 2,
            contract: "investigation-completion-obligations@2".into(),
            obligations: vec![item],
        },
        inspection_requirements: None,
        question: "Which limits are established?".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: "investigation-v2".into(),
        expected_graph_revision: None,
        expected_context_id: context_id,
        conversation_id: None,
        parent_id: None,
    }
}

#[test]
fn admission_family_internal_request_seven_has_distinct_hash_and_unchanged_envelope() {
    // AC-0287 / T-0287: the registered family grants no larger budget.
    let policy = &INVESTIGATION_V2_POLICY;
    for name in ["domain-analyst", "evidence-auditor"] {
        let future = request(&format!("{name}@10"));
        let normalized = future.normalized_with_policy(policy).unwrap();
        let bytes = serde_json::to_vec(&future).unwrap();
        assert_eq!(serde_json::to_vec(&normalized).unwrap(), bytes);
        assert_eq!(
            future.intent_hash_with_policy(policy).unwrap(),
            crate::source_basis::domain_hash(
                b"cartograph:investigation-client-intent:v7\0",
                &bytes
            )
        );
        let mut current = future.clone();
        current.schema_version = 6;
        current.specialist_id = format!("{name}@9");
        assert_eq!(
            current.obligations.fingerprint_with_policy(policy).unwrap(),
            future.obligations.fingerprint_with_policy(policy).unwrap()
        );
        assert_ne!(
            current.intent_hash_with_policy(policy).unwrap(),
            future.intent_hash_with_policy(policy).unwrap()
        );
        assert!(
            future
                .normalized_with_policy(&INVESTIGATION_V1_POLICY)
                .is_err()
        );
    }
    assert_eq!(policy.request_bytes(), 65536);
    assert_eq!(policy.max_completion_obligations(), 56);
    assert_eq!(policy.limits().model_invocations, 64);
    assert_eq!(policy.limits().tool_actions, 56);
}

#[test]
fn admission_family_public_raw_and_ipc_accept_exact_request_seven_tuple() {
    // AC-0287 / T-0287, AC-0600: activation changes only the exact family discriminator.
    let policy = &INVESTIGATION_V2_POLICY;
    for role in [
        "domain-analyst@10",
        "evidence-auditor@10",
        "evidence-auditor@13",
        "evidence-auditor@14",
        "evidence-auditor@15",
    ] {
        let current = request(role);
        let raw = serde_json::to_string(&current).unwrap();
        assert!(StagedObligationRequest::decode(&raw).is_err()); // v1 default
        assert_eq!(
            serde_json::to_string(
                &StagedObligationRequest::decode_with_policy(&raw, policy).unwrap()
            )
            .unwrap(),
            raw
        );
        assert_eq!(
            serde_json::to_string(&InvestigationStartRequest::decode(&raw).unwrap()).unwrap(),
            raw
        );
        let ipc: InvestigationStartRequest = serde_json::from_str(&raw).unwrap();
        let InvestigationStartRequest::Completion(ipc) = ipc else {
            panic!("request7 decoded as legacy")
        };
        assert_eq!(
            serde_json::to_string(&ipc.normalized_with_policy(policy).unwrap()).unwrap(),
            raw
        );
        // A future role never enters the legacy StartInvestigationRequest arm.
        let mut legacy = serde_json::to_value(&current).unwrap();
        legacy.as_object_mut().unwrap().remove("obligations");
        for schema in 1..=3 {
            legacy["schema_version"] = schema.into();
            let value: StartInvestigationRequest = serde_json::from_value(legacy.clone()).unwrap();
            assert!(value.validate().is_err());
            assert!(InvestigationStartRequest::decode(&legacy.to_string()).is_err());
        }
    }
}

#[test]
fn admission_family_public_decoder_rejects_mixed_tuples_and_unknown_authority() {
    // IPC decodes recognized closed shapes; semantic rejection remains at the
    // host's normalization boundary so it can report structured invalid_input.
    for role in ["domain-analyst@10", "evidence-auditor@10"] {
        let value = serde_json::to_value(request(role)).unwrap();
        let mut mismatches = Vec::new();
        for schema in 4..=6 {
            let mut wrong = value.clone();
            wrong["schema_version"] = schema.into();
            mismatches.push(wrong);
        }
        for role in [
            "domain-analyst@7",
            "evidence-auditor@8",
            "domain-analyst@9",
            "evidence-auditor@16",
        ] {
            let mut wrong = value.clone();
            wrong["specialist_id"] = role.into();
            mismatches.push(wrong);
        }
        let mut wrong = value.clone();
        wrong["limit_profile"] = "investigation-v1".into();
        mismatches.push(wrong);
        let mut wrong = value.clone();
        wrong["obligations"]["schema_version"] = 1.into();
        mismatches.push(wrong);
        let mut wrong = value.clone();
        wrong["obligations"]["contract"] = CONTRACT.into();
        mismatches.push(wrong);
        for wrong in mismatches {
            let raw = wrong.to_string();
            assert!(InvestigationStartRequest::decode(&raw).is_err(), "{raw}");
            assert!(
                StagedObligationRequest::decode_with_policy(&raw, &INVESTIGATION_V2_POLICY)
                    .is_err()
            );
            let ipc: InvestigationStartRequest = serde_json::from_value(wrong).unwrap();
            let InvestigationStartRequest::Completion(ipc) = ipc else {
                panic!("wrong IPC family")
            };
            for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
                assert!(ipc.normalized_with_policy(policy).is_err());
            }
        }
        for field in ["policy_limits", "query_history", "prompt_schema"] {
            let mut wrong = value.clone();
            wrong[field] = serde_json::json!({"tool_actions":999});
            assert!(InvestigationStartRequest::decode(&wrong.to_string()).is_err());
            assert!(serde_json::from_value::<InvestigationStartRequest>(wrong).is_err());
        }
        for schema in [0, 8, 99] {
            let mut wrong = value.clone();
            wrong["schema_version"] = schema.into();
            assert!(InvestigationStartRequest::decode(&wrong.to_string()).is_err());
            assert!(serde_json::from_value::<InvestigationStartRequest>(wrong).is_err());
        }
    }
}

#[test]
fn admission_family_public_raw_preflight_preserves_duplicate_and_byte_bounds() {
    let policy = &INVESTIGATION_V2_POLICY;
    for role in ["domain-analyst@10", "evidence-auditor@10"] {
        let raw = serde_json::to_string(&request(role)).unwrap();
        for extra in [
            "\"schema_version\":7,",
            "\"specialist_id\":\"domain-analyst@10\",",
        ] {
            let duplicate = format!("{{{extra}{}", &raw[1..]);
            assert!(InvestigationStartRequest::decode(&duplicate).is_err());
            assert!(StagedObligationRequest::decode_with_policy(&duplicate, policy).is_err());
        }
        let at_cap = format!("{raw}{}", " ".repeat(policy.request_bytes() - raw.len()));
        assert!(InvestigationStartRequest::decode(&at_cap).is_ok());
        assert!(StagedObligationRequest::decode_with_policy(&at_cap, policy).is_ok());
        assert!(matches!(
            InvestigationStartRequest::decode(&(at_cap + " ")),
            Err(InvestigationError::LimitExceeded)
        ));
    }
}

#[test]
fn admission_family_public_decoder_preserves_existing_completion_wires() {
    // AC-0287 / T-0287: activation never changes published older request bytes.
    for (schema, version, profile) in [
        (4, 7, "investigation-v1"),
        (5, 8, "investigation-v2"),
        (6, 9, "investigation-v2"),
    ] {
        let policy = InvestigationPolicy::for_profile(profile).unwrap();
        for name in ["domain-analyst", "evidence-auditor"] {
            let mut value = request(&format!("{name}@{version}"));
            value.schema_version = schema;
            value.limit_profile = profile.into();
            if schema == 4 {
                value.obligations.schema_version = 1;
                value.obligations.contract = CONTRACT.into();
            }
            let raw = serde_json::to_string(&value).unwrap();
            let decoded = StagedObligationRequest::decode_with_policy(&raw, policy).unwrap();
            assert_eq!(serde_json::to_string(&decoded).unwrap(), raw);
            assert_eq!(
                serde_json::to_string(&InvestigationStartRequest::decode(&raw).unwrap()).unwrap(),
                raw
            );
        }
    }
}

#[test]
fn spec_writer_v1_is_admitted_only_in_the_exact_standard_schema_four_family() {
    // AC-0382 / T-0382: the role version is independent from protocol schema.
    let policy = &INVESTIGATION_V1_POLICY;
    let mut request = request("spec-writer@1");
    request.schema_version = 4;
    request.obligations.schema_version = 1;
    request.obligations.contract = CONTRACT.into();
    request.limit_profile = "investigation-v1".into();
    let raw = serde_json::to_string(&request).unwrap();
    assert_eq!(
        serde_json::to_vec(&StagedObligationRequest::decode(&raw).unwrap()).unwrap(),
        serde_json::to_vec(&request).unwrap()
    );
    let decoded = InvestigationStartRequest::decode(&raw).unwrap();
    let InvestigationStartRequest::Completion(decoded) = decoded else {
        panic!("schema-four Spec Writer request decoded as legacy")
    };
    assert_eq!(
        serde_json::to_vec(&decoded).unwrap(),
        serde_json::to_vec(&request).unwrap()
    );
    assert_eq!(
        request.intent_hash_with_policy(policy).unwrap(),
        crate::source_basis::domain_hash(
            b"cartograph:investigation-client-intent:v4\0",
            &serde_json::to_vec(&request).unwrap()
        )
    );
    for (schema, profile) in [
        (3, "investigation-v1"),
        (5, "investigation-v2"),
        (4, "investigation-v2"),
    ] {
        let mut invalid = request.clone();
        invalid.schema_version = schema;
        invalid.limit_profile = profile.into();
        if schema == 5 {
            invalid.obligations.schema_version = 2;
            invalid.obligations.contract = "investigation-completion-obligations@2".into();
        }
        let selected = InvestigationPolicy::for_profile(profile).unwrap();
        assert!(invalid.normalized_with_policy(selected).is_err());
    }
    let mut invalid_role = request;
    invalid_role.specialist_id = "spec-writer@7".into();
    assert!(invalid_role.normalized_with_policy(policy).is_err());
}

#[test]
fn gap_triager_v1_is_admitted_only_in_the_exact_standard_schema_four_family() {
    gap_triager_standard_family("gap-triager@1");
}

// AC-0406: new role keeps the exact Standard tuple; Expanded and unknown remain closed.
#[test]
fn gap_triager_v2_is_admitted_only_in_the_exact_standard_schema_four_family() {
    gap_triager_standard_family("gap-triager@2");
}

fn gap_triager_standard_family(role: &str) {
    // AC-0384: the new role preserves the v4 intent domain and does not enroll elsewhere.
    let policy = &INVESTIGATION_V1_POLICY;
    let mut request = request(role);
    request.schema_version = 4;
    request.obligations.schema_version = 1;
    request.obligations.contract = CONTRACT.into();
    request.limit_profile = "investigation-v1".into();
    let raw = serde_json::to_string(&request).unwrap();
    assert_eq!(
        serde_json::to_vec(&StagedObligationRequest::decode(&raw).unwrap()).unwrap(),
        serde_json::to_vec(&request).unwrap()
    );
    assert!(matches!(
        InvestigationStartRequest::decode(&raw).unwrap(),
        InvestigationStartRequest::Completion(_)
    ));
    assert_eq!(
        request.intent_hash_with_policy(policy).unwrap(),
        crate::source_basis::domain_hash(
            b"cartograph:investigation-client-intent:v4\0",
            &serde_json::to_vec(&request).unwrap()
        )
    );
    let mut wrong_role = request.clone();
    wrong_role.specialist_id = "gap-triager@10".into();
    assert!(wrong_role.normalized_with_policy(policy).is_err());
    let mut wrong_profile = request;
    wrong_profile.limit_profile = "investigation-v2".into();
    assert!(
        wrong_profile
            .normalized_with_policy(&INVESTIGATION_V2_POLICY)
            .is_err()
    );
}
#[test]
fn admission_family_specialists_add_only_internal_admission_instructions() {
    // AC-0287 / T-0287: @9 text and operations retain their published meaning.
    for (old, new) in [
        (
            SpecialistId::DomainAnalystV9,
            SpecialistId::DomainAnalystV10,
        ),
        (
            SpecialistId::EvidenceAuditorV9,
            SpecialistId::EvidenceAuditorV10,
        ),
    ] {
        let old_definition = old.definition().unwrap();
        let definition = new.definition().unwrap();
        assert!(!old.uses_next_prompt_admission());
        assert!(new.uses_next_prompt_admission());
        assert_eq!(definition.version, 10);
        assert_eq!(definition.operations, old_definition.operations);
        assert_ne!(
            definition.prompt_fingerprint,
            old_definition.prompt_fingerprint
        );
        assert!(new.uses_query_history_disclosure());
        assert!(new.uses_completion_obligations());
        assert!(new.uses_policy_selection());
        let prompt = new.prompt();
        assert!(prompt.starts_with(&old.prompt()));
        assert!(!old.prompt().contains("finish-only mode"));
        assert!(prompt.contains("finish-only mode"));
        assert!(prompt.contains("A sizing envelope is not actual feedback or permission to act"));
        let wire = serde_json::to_string(&new).unwrap();
        assert_eq!(serde_json::from_str::<SpecialistId>(&wire).unwrap(), new);
    }
}

// AC-0432: the new role keeps request7/envelope2 and cannot alias saved @10 intent.
#[test]
fn progress_guidance_roles_reuse_request_seven_with_distinct_intents() {
    let policy = &INVESTIGATION_V2_POLICY;
    for name in ["domain-analyst", "evidence-auditor"] {
        let old = request(&format!("{name}@10"));
        let new = request(&format!("{name}@11"));
        let raw = serde_json::to_string(&new).unwrap();
        assert!(InvestigationStartRequest::decode(&raw).is_ok());
        assert_eq!(
            new.normalized_with_policy(policy).unwrap().schema_version,
            7
        );
        assert_eq!(new.obligations, old.obligations);
        assert_ne!(
            new.intent_hash_with_policy(policy).unwrap(),
            old.intent_hash_with_policy(policy).unwrap()
        );
        let mut wrong = new.clone();
        wrong.schema_version = 6;
        assert!(wrong.normalized_with_policy(policy).is_err());
        assert!(
            new.normalized_with_policy(&INVESTIGATION_V1_POLICY)
                .is_err()
        );
    }
}
