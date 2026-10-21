use super::*;

fn request(schema: u32, role: &str, profile: &str) -> StagedObligationRequest {
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
        schema_version: schema,
        request_nonce: "family-fixture-1".into(),
        specialist_id: role.into(),
        obligations: ObligationEnvelope {
            schema_version: if profile == "investigation-v1" { 1 } else { 2 },
            contract: if profile == "investigation-v1" {
                CONTRACT
            } else {
                "investigation-completion-obligations@2"
            }
            .into(),
            obligations: vec![item],
        },
        inspection_requirements: None,
        question: "Which limits are established?".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: profile.into(),
        expected_graph_revision: None,
        expected_context_id: context_id,
        conversation_id: None,
        parent_id: None,
    }
}

#[test]
fn query_history_request_family_requires_exact_schema_role_and_policy_tuple() {
    // AC-0286 / T-0286: policy selection alone never chooses a prompt family.
    for profile in ["investigation-v1", "investigation-v2"] {
        let policy = InvestigationPolicy::for_profile(profile).unwrap();
        for schema in 4..=7 {
            for version in 7..=10 {
                for name in ["domain-analyst", "evidence-auditor"] {
                    let role = format!("{name}@{version}");
                    let value = request(schema, &role, profile);
                    let expected = matches!(
                        (profile, schema, version),
                        ("investigation-v1", 4, 7)
                            | ("investigation-v2", 5, 8)
                            | ("investigation-v2", 6, 9)
                            | ("investigation-v2", 7, 10)
                    );
                    assert_eq!(
                        value.normalized_with_policy(policy).is_ok(),
                        expected,
                        "{profile}/{schema}/{role}"
                    );
                    let raw = serde_json::to_string(&value).unwrap();
                    assert_eq!(
                        StagedObligationRequest::decode_with_policy(&raw, policy).is_ok(),
                        expected && schema <= 6
                    );
                    assert_eq!(value.intent_hash_with_policy(policy).is_ok(), expected);
                    if schema == 7 {
                        // AC-0287: every internal/invalid role-policy tuple is
                        // still rejected by both public start entrypoints.
                        assert!(InvestigationStartRequest::decode(&raw).is_err());
                        assert!(serde_json::from_str::<InvestigationStartRequest>(&raw).is_err());
                    }
                }
            }
        }
    }
}

#[test]
fn query_history_keeps_envelope_two_and_v2_numeric_admission_unchanged() {
    // AC-0286 / T-0286: new disclosure semantics never grant larger budgets.
    let policy = InvestigationPolicy::for_profile("investigation-v2").unwrap();
    let old = request(5, "domain-analyst@8", policy.profile());
    let current = request(6, "domain-analyst@9", policy.profile());
    assert_eq!(
        old.obligations.fingerprint_with_policy(policy).unwrap(),
        current.obligations.fingerprint_with_policy(policy).unwrap()
    );
    assert_eq!(policy.request_bytes(), 65536);
    assert_eq!(policy.max_completion_obligations(), 56);
    assert_eq!(policy.limits().model_invocations, 64);
    assert_eq!(policy.limits().tool_actions, 56);
    assert_eq!(policy.max_completed_attempts(), 112);
    let raw = serde_json::to_string(&current).unwrap();
    let at_cap = format!("{raw}{}", " ".repeat(policy.request_bytes() - raw.len()));
    StagedObligationRequest::decode_with_policy(&at_cap, policy).unwrap();
    assert!(StagedObligationRequest::decode_with_policy(&(at_cap + " "), policy).is_err());
    let mut wrong = current.clone();
    wrong.obligations.schema_version = 1;
    assert!(wrong.normalized_with_policy(policy).is_err());
    let mut legacy = current;
    legacy.schema_version = 5;
    assert!(legacy.normalized_with_policy(policy).is_err());
}

#[test]
fn query_history_new_request_domain_preserves_old_family_domains_and_wires() {
    // AC-0286 / T-0286: old intents retain both their bytes and domain meaning.
    for (schema, role, profile, domain) in [
        (
            4,
            "domain-analyst@7",
            "investigation-v1",
            b"cartograph:investigation-client-intent:v4\0".as_slice(),
        ),
        (
            5,
            "domain-analyst@8",
            "investigation-v2",
            b"cartograph:investigation-client-intent:v5\0".as_slice(),
        ),
        (
            6,
            "domain-analyst@9",
            "investigation-v2",
            b"cartograph:investigation-client-intent:v6\0".as_slice(),
        ),
    ] {
        let policy = InvestigationPolicy::for_profile(profile).unwrap();
        let value = request(schema, role, profile);
        let bytes = serde_json::to_vec(&value).unwrap();
        let normalized = value.normalized_with_policy(policy).unwrap();
        assert_eq!(serde_json::to_vec(&normalized).unwrap(), bytes);
        assert_eq!(
            value.intent_hash_with_policy(policy).unwrap(),
            crate::source_basis::domain_hash(domain, &bytes)
        );
        let decoded = StagedObligationRequest::decode_with_policy(
            std::str::from_utf8(&bytes).unwrap(),
            policy,
        )
        .unwrap();
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), bytes);
    }
}

#[test]
fn query_history_family_rejects_unknown_and_legacy_authority_fields() {
    // AC-0286 / T-0286: projections cannot discard extra request authority.
    let policy = InvestigationPolicy::for_profile("investigation-v2").unwrap();
    let value = request(6, "evidence-auditor@9", policy.profile());
    for extra in [
        "\"inspection_requirements\":null,",
        "\"query_history\":[],",
        "\"prompt_schema\":7,",
    ] {
        let raw = serde_json::to_string(&value).unwrap();
        let altered = format!("{{{extra}{}", &raw[1..]);
        assert!(StagedObligationRequest::decode_with_policy(&altered, policy).is_err());
    }
    assert!(StagedObligationRequest::decode(&serde_json::to_string(&value).unwrap()).is_err());
}

#[test]
fn query_history_family_preserves_capacity_error_before_invalid_old_role() {
    // AC-0286 / T-0286: adding a family never changes old admission ordering.
    for profile in ["investigation-v1", "investigation-v2"] {
        let policy = InvestigationPolicy::for_profile(profile).unwrap();
        let mut value = request(4, "unknown-role@7", profile);
        assert!(matches!(
            value.normalized_with_policy(policy),
            Err(InvestigationError::InvalidRequest)
        ));
        assert!(matches!(
            value.intent_hash_with_policy(policy),
            Err(InvestigationError::InvalidRequest)
        ));
        value.question = "x".repeat(policy.request_bytes());
        assert!(matches!(
            value.normalized_with_policy(policy),
            Err(InvestigationError::LimitExceeded)
        ));
        assert!(matches!(
            value.intent_hash_with_policy(policy),
            Err(InvestigationError::LimitExceeded)
        ));
    }
}

#[test]
fn query_history_specialists_have_separate_immutable_prompts_and_operations() {
    // AC-0286 / T-0286: only @9 advertises fixed references and charged replay.
    for (old, new) in [
        (SpecialistId::DomainAnalystV8, SpecialistId::DomainAnalystV9),
        (
            SpecialistId::EvidenceAuditorV8,
            SpecialistId::EvidenceAuditorV9,
        ),
    ] {
        let older = old.definition().unwrap();
        let current = new.definition().unwrap();
        assert_eq!(current.version, 9);
        assert_ne!(older.prompt_fingerprint, current.prompt_fingerprint);
        assert!(!old.uses_query_history_disclosure());
        assert!(new.uses_query_history_disclosure());
        assert!(!older.operations.iter().any(|op| op == "replay_query"));
        assert_eq!(
            current
                .operations
                .iter()
                .filter(|op| *op == "replay_query")
                .count(),
            1
        );
        let prompt = new.prompt();
        assert!(prompt.contains("fixed-size ordered query references"));
        assert!(prompt.contains("Only the latest query"));
        assert!(prompt.contains("normal tool-action and raw-response budgets"));
        assert!(prompt.contains("References alone cannot supply evidence or discharge"));
        assert!(prompt.contains("at most64 model invocations and56 tool actions"));
        assert!(!prompt.contains("Each query records ordered fact_indexes"));
        assert!(new.uses_completion_obligations());
        assert!(new.uses_coverage_qualification());
        assert!(new.uses_turn_progress());
        assert!(new.uses_compact_disclosure());
        assert!(new.uses_policy_selection());
    }
}

#[test]
fn query_history_ipc_deserializer_accepts_current_and_historical_completion_families() {
    // AC-0286: Tauri's parsed-Value path must accept the catalog's schema6.
    for (schema, version, profile) in [
        (4, 7, "investigation-v1"),
        (5, 8, "investigation-v2"),
        (6, 9, "investigation-v2"),
    ] {
        for name in ["domain-analyst", "evidence-auditor"] {
            let expected = request(schema, &format!("{name}@{version}"), profile);
            let raw = serde_json::to_value(&expected).unwrap();
            let decoded: InvestigationStartRequest = serde_json::from_value(raw.clone()).unwrap();
            assert!(matches!(decoded, InvestigationStartRequest::Completion(_)));
            assert_eq!(serde_json::to_value(&decoded).unwrap(), raw);
            assert!(InvestigationStartRequest::decode(&raw.to_string()).is_ok());
            let mut extra = raw.clone();
            extra["unknown_authority"] = serde_json::json!(true);
            assert!(serde_json::from_value::<InvestigationStartRequest>(extra).is_err());
        }
    }
    let mut future =
        serde_json::to_value(request(6, "domain-analyst@9", "investigation-v2")).unwrap();
    future["schema_version"] = serde_json::json!(7);
    assert!(serde_json::from_value::<InvestigationStartRequest>(future).is_err());
}
