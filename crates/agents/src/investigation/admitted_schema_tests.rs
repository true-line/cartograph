//! AC-0447: local preparation only; no installed grammar/provider acceptance.
use super::*;
use llm::{EgressFirewall, EgressPolicy, LlmProvider, OllamaProvider};

fn schema(input: &InvestigationInput) -> Value {
    serde_json::from_str(input.payload.response_schema.as_deref().unwrap()).unwrap()
}
fn read_branch(schema: &Value) -> Option<&Value> {
    schema["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["properties"]["type"]["const"] == "read_evidence")
}

#[test]
fn admitted_source_schema_tracks_bound_facts_and_exact_preparation() {
    let role = SpecialistId::DomainAnalystV14;
    let policy = &INVESTIGATION_V2_POLICY;
    let mode = CompletionCallMode::Choice;
    let fresh = Fixture::new(role, 7)
        .build(role, &usage(mode), mode, 0, None)
        .unwrap();
    assert!(read_branch(&schema(&fresh)).is_none());
    let (fixture, raw, target, _) = source_fixture(role, "ordinary source");
    let input = build_source(&fixture, &raw, &[], role, mode);
    assert_eq!(
        read_branch(&schema(&input)).unwrap()["properties"]["fact"],
        json!({"enum":[target.fact]})
    );
    assert!(input.validate_admitted_source_fact(&target.fact).is_ok());
    assert_eq!(
        fresh
            .validate_admitted_source_fact(&target.fact)
            .unwrap_err()
            .stage,
        ActionDecodeStage::SemanticRule(ActionSemanticRule::EvidenceShape)
    );
    let unknown = crate::TaskFactKey::Node {
        id: "unknown".into(),
    };
    assert!(input.validate_admitted_source_fact(&unknown).is_err());

    // The same fact key without a frozen binding never grants a source option.
    let mut unbound = input.clone();
    unbound.ledger.selected_facts[0].binding = None;
    let without_binding = response_schema::select_admitted(role, mode, &unbound.ledger)
        .unwrap()
        .unwrap();
    assert!(read_branch(&serde_json::from_str::<Value>(&without_binding).unwrap()).is_none());
    assert!(unbound.validate_admitted_source_fact(&target.fact).is_err());
    // A changed ledger retaining the same admitted keys selects the same schema.
    let mut revised = input.ledger.clone();
    revised.revision += 1;
    assert_eq!(
        response_schema::select_admitted(role, mode, &revised).unwrap(),
        input.payload.response_schema
    );

    let mut changed_keys = input.ledger.clone();
    changed_keys.selected_facts[0].fact = unknown;
    assert_ne!(
        response_schema::select_admitted(role, mode, &changed_keys).unwrap(),
        input.payload.response_schema
    );
    assert!(response_schema::validate(&changed_keys, &input.payload, &prompt(&input)).is_err());
    assert!(response_schema::validate(&unbound.ledger, &input.payload, &prompt(&input)).is_err());
    // The selector has its own bounded encoding even if internal growth is invalid.
    let mut excessive = input.ledger.clone();
    excessive.selected_facts[0].fact = crate::TaskFactKey::Node {
        id: "x".repeat(MAX_INPUT_BYTES),
    };
    assert_eq!(
        response_schema::select_admitted(role, mode, &excessive),
        Err(InvestigationError::LimitExceeded)
    );

    let provider = OllamaProvider::local_default().unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let limits = llm::bounded::CompletionLimits::default();
    for current in [&fresh, &input] {
        let prepared = firewall
            .prepare_bounded(
                &provider,
                &current.action("schema:current".into()).unwrap(),
                &limits,
            )
            .unwrap();
        assert!(
            current
                .seal_disclosure_with_policy(&prepared, policy)
                .unwrap()
                .is_some()
        );
        let reserve = current
            .measure_v9_reserve_with_policy(
                &provider.bounded_profile().unwrap(),
                &limits,
                "schema:next",
                policy,
            )
            .unwrap();
        assert!(reserve.choice.original_payload_bytes > reserve.finish_only.original_payload_bytes);
        for replacement in [
            None,
            Some(response_schema::CHOICE.into()),
            Some(response_schema::FINISH.into()),
            if current.ledger.selected_facts.is_empty() {
                input.payload.response_schema.clone()
            } else {
                fresh.payload.response_schema.clone()
            },
        ] {
            let mut wrong = current.clone();
            wrong.payload.response_schema = replacement;
            assert!(wrong.admission_reserve_basis(policy).is_err());
            let prepared = firewall
                .prepare_bounded(
                    &provider,
                    &wrong.action("schema:wrong".into()).unwrap(),
                    &limits,
                )
                .unwrap();
            assert!(
                wrong
                    .seal_disclosure_with_policy(&prepared, policy)
                    .is_err()
            );
        }
    }
    // A selected source exercises both source-preview reserve modes and sealing.
    let prepared = firewall
        .prepare_bounded(
            &provider,
            &input.action("schema:current".into()).unwrap(),
            &limits,
        )
        .unwrap();
    input
        .certify_unread_source_v9_with_policy(
            &prepared,
            &target,
            &provider.bounded_profile().unwrap(),
            &limits,
            "schema:next",
            policy,
        )
        .unwrap();
    let finish = build_source(&fixture, &raw, &[], role, CompletionCallMode::FinishOnly);
    assert_eq!(
        finish.payload.response_schema.as_deref(),
        Some(response_schema::FINISH)
    );

    // Historical @13's prompt is exactly its original expression and templates.
    let expected = format!(
        "{}\nCompiled analyst response schemas: choice={} finish_only={}. Host validation remains authoritative.",
        SpecialistId::DomainAnalystV12.prompt(),
        content_hash(response_schema::CHOICE.as_bytes()),
        content_hash(response_schema::FINISH.as_bytes())
    );
    assert_eq!(SpecialistId::DomainAnalystV13.prompt(), expected);
    assert_eq!(
        SpecialistId::DomainAnalystV13
            .definition()
            .unwrap()
            .prompt_fingerprint,
        crate::source_basis::domain_hash(
            b"cartograph:specialist-definition:v1\0",
            &bounded_bytes(
                &(SpecialistId::DomainAnalystV13, 13u32, &expected),
                16 * 1024
            )
            .unwrap()
        )
    );
    assert_eq!(
        response_schema::select_admitted(SpecialistId::DomainAnalystV13, mode, &fresh.ledger)
            .unwrap(),
        Some(response_schema::CHOICE.into())
    );
}
