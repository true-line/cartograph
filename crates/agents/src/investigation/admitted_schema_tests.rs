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
    check_admitted_source_schema(SpecialistId::DomainAnalystV14);
}

// AC-0449: prompt identity and local admission mechanics, not model acceptance.
#[test]
fn analyst_phase_discovery_preserves_history_and_schema() {
    let old = SpecialistId::DomainAnalystV14;
    let new = SpecialistId::DomainAnalystV15;
    assert_eq!(
        old.definition().unwrap().prompt_fingerprint,
        "b1634f6fb40d19c2ddc58fb8845e1f7f66bd6133d04c8a7e491df5910c857ffd"
    );
    assert_eq!(
        new.definition().unwrap().operations,
        old.definition().unwrap().operations
    );
    let prompt = new.prompt();
    assert!(prompt.len() >= old.prompt().len()); // retained @14 fits the @15 workspace owner
    assert!(old.prompt().contains("Query first to discover facts;"));
    assert!(!prompt.contains("Query first to discover facts;"));
    assert!(
        old.prompt()
            .contains(r#""max_facts":12,"max_bytes":16384,"cursor":null"#)
    );
    assert!(!prompt.contains(
        r#"{"type":"query_context","context_id":"the exact context_id supplied by the host","query"#
    ));
    for instruction in [
        "No query selector is a default",
        "Only turn_progress.phase initial",
        "an exact returned continuation",
        "a different useful query",
        "coverage inventory",
        "an admissible finish",
        "unchanged no-progress query",
        "not acquired source",
        "per-call schema specializes the compiled templates",
        "You choose the next action",
    ] {
        assert!(prompt.contains(instruction), "{instruction}");
    }
    let ledger = Fixture::new(new, 7).ledger;
    for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
        assert_eq!(
            response_schema::select_admitted(new, mode, &ledger).unwrap(),
            response_schema::select_admitted(old, mode, &ledger).unwrap()
        );
    }
    check_admitted_source_schema(new);
}

fn check_admitted_source_schema(role: SpecialistId) {
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

// AC-0461: the new query capability does not change task scopes or historical decoders.
#[test]
fn exact_node_analyst_capability_is_closed_for_historical_actions_and_replay() {
    assert_eq!(
        SpecialistId::DomainAnalystV15
            .definition()
            .unwrap()
            .prompt_fingerprint,
        "ec99aee7d993cae30220094c8afb31430ba33a95b77cd4389ae81fbe0ef0041c"
    );
    let role = SpecialistId::DomainAnalystV17;
    let policy = &INVESTIGATION_V2_POLICY;
    let (mut fixture, raw, target, _) = source_fixture(role, "ordinary source");
    let crate::TaskFactKey::Node { id } = &target.fact else {
        unreachable!()
    };
    let node = InvestigationScope::Node { id: id.clone() };
    assert!(node.validate().is_err());
    let mut request = fixture.request.clone();
    request.scope = node.clone();
    assert!(request.normalized_with_policy(policy).is_err());
    fixture.ledger.queries[0].query.scope = node;
    fixture.basis.input_ledger_hash = fixture.ledger.fingerprint_with_policy(policy).unwrap();
    fixture
        .ledger
        .validate_for_specialist(role, policy)
        .unwrap();
    let input = build_source(&fixture, &raw, &[], role, CompletionCallMode::Choice);
    let current = schema(&input);
    assert!(
        current["$defs"]["query_scope"]["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["properties"]["type"]["const"] == "node")
    );
    assert_eq!(
        read_branch(&current).unwrap()["properties"]["fact"],
        json!({"enum":[target.fact]})
    );
    let context = &fixture.request.expected_context_id;
    let hash = &fixture.metadata.request_hash;
    let query = json!({"type":"query_context","context_id":context,"query":fixture.ledger.queries[0].query});
    let decode = |value: &Value, role| {
        ProgressiveInvestigationAction::decode_resolved_for_specialist(
            &value.to_string(),
            context,
            hash,
            &fixture.ledger,
            policy,
            role,
        )
    };
    assert!(decode(&query, role).is_ok());
    assert!(QualifiedInvestigationAction::decode(&query.to_string(), context).is_err());
    assert!(CompletionInvestigationAction::decode(&query.to_string(), context).is_err());
    let mut legacy = query.clone();
    legacy.as_object_mut().unwrap().remove("context_id");
    assert!(InvestigationAction::decode(&legacy.to_string()).is_err());
    for old in [
        SpecialistId::DomainAnalystV10,
        SpecialistId::DomainAnalystV12,
        SpecialistId::DomainAnalystV14,
        SpecialistId::DomainAnalystV15,
        SpecialistId::EvidenceAuditorV11,
    ] {
        assert!(decode(&query, old).is_err());
        assert!(fixture.ledger.validate_for_specialist(old, policy).is_err());
        let mut mismatched = input.clone();
        mismatched.payload.system = old.prompt();
        assert!(mismatched.admission_reserve_basis(policy).is_err());
    }
    for (pointer, bad) in [
        ("/query/scope/id", json!("")),
        ("/query/scope/id", json!("x".repeat(1025))),
        ("/context_id", json!("wrong")),
        ("/query/max_facts", json!(0)),
        ("/query/scope/extra", json!(1)),
    ] {
        let mut invalid = query.clone();
        if pointer.ends_with("extra") {
            invalid["query"]["scope"]["extra"] = bad;
        } else {
            *invalid.pointer_mut(pointer).unwrap() = bad;
        }
        assert!(decode(&invalid, role).is_err());
    }
    let mut omitted = query.clone();
    omitted["query"].as_object_mut().unwrap().remove("cursor");
    assert!(decode(&omitted, role).is_err());
    let duplicate = query
        .to_string()
        .replace("\"scope\":", "\"scope\":{},\"scope\":");
    assert!(
        ProgressiveInvestigationAction::decode_resolved_for_specialist(
            &duplicate,
            context,
            hash,
            &fixture.ledger,
            policy,
            role
        )
        .is_err()
    );
    let reference = QueryHistoryReference::from_ledger(hash, &fixture.ledger, 0, policy).unwrap();
    let replay = json!({"type":"replay_query","context_id":context,"query_index":reference.query_index,"query_id":reference.query_id});
    assert!(decode(&replay, role).is_ok());
    assert!(decode(&replay, SpecialistId::DomainAnalystV15).is_err());
    assert!(
        ProgressiveInvestigationAction::decode_resolved(
            &replay.to_string(),
            context,
            hash,
            &fixture.ledger,
            policy
        )
        .is_err()
    );
    assert_eq!(
        response_schema::select_admitted(role, CompletionCallMode::FinishOnly, &fixture.ledger)
            .unwrap(),
        Some(response_schema::FINISH.into())
    );
}
