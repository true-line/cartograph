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
    // AC-0287 / T-0287: constructing the future family grants no larger budget.
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
fn admission_family_public_raw_and_ipc_entries_reject_request_seven() {
    // AC-0287 / T-0287: internal identity scaffolding cannot activate IPC.
    for role in ["domain-analyst@10", "evidence-auditor@10"] {
        let future = request(role);
        future
            .normalized_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        let raw = serde_json::to_string(&future).unwrap();
        assert!(StagedObligationRequest::decode(&raw).is_err());
        assert!(
            StagedObligationRequest::decode_with_policy(&raw, &INVESTIGATION_V2_POLICY).is_err()
        );
        assert!(InvestigationStartRequest::decode(&raw).is_err());
        assert!(serde_json::from_str::<InvestigationStartRequest>(&raw).is_err());
        // Re-labelling the generation cannot smuggle an internal role through
        // an otherwise available public completion request family.
        for schema in 4..=6 {
            let mut mismatch = future.clone();
            mismatch.schema_version = schema;
            let raw = serde_json::to_string(&mismatch).unwrap();
            assert!(InvestigationStartRequest::decode(&raw).is_err());
            assert!(
                StagedObligationRequest::decode_with_policy(&raw, &INVESTIGATION_V2_POLICY)
                    .is_err()
            );
        }
        // The shared identity DTO must reopen internal saved data, but it does
        // not grant an old-style request the new specialist's authority.
        let mut legacy = serde_json::to_value(&future).unwrap();
        legacy.as_object_mut().unwrap().remove("obligations");
        for schema in 1..=3 {
            legacy["schema_version"] = schema.into();
            let value: StartInvestigationRequest = serde_json::from_value(legacy.clone()).unwrap();
            assert!(value.validate().is_err());
        }
    }
}

#[test]
fn admission_family_public_decoder_preserves_existing_completion_wires() {
    // AC-0287 / T-0287: the internal family never changes published request bytes.
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
