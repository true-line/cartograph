//! AC-0454: exact local representation/admission checks; no provider execution.
use super::*;
use llm::bounded::{CompletionLimits, FailureCode, inspect_bounded_action};
use llm::{EgressFirewall, EgressPolicy, LlmProvider, OllamaProvider};

const ROLE: SpecialistId = SpecialistId::DomainAnalystV16;

fn assert_visible(input: &InvestigationInput) {
    let body = prompt(input);
    assert_eq!(
        bounded_bytes(&body["response_schema"], MAX_INPUT_BYTES).unwrap(),
        input.payload.response_schema.as_ref().unwrap().as_bytes()
    );
    input
        .admission_reserve_basis(&INVESTIGATION_V2_POLICY)
        .unwrap();
}

#[test]
fn visible_schema_matches_selected_modes_and_rejects_tampering() {
    let policy = &INVESTIGATION_V2_POLICY;
    let provider = OllamaProvider::local_default().unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let limits = CompletionLimits::default();
    let fresh = Fixture::new(ROLE, 7)
        .build(
            ROLE,
            &usage(CompletionCallMode::Choice),
            CompletionCallMode::Choice,
            0,
            None,
        )
        .unwrap();
    let (mut fixture, raw, target, citation) = source_fixture(ROLE, "ordinary source");
    let queried = build_source(&fixture, &raw, &[], ROLE, CompletionCallMode::Choice);
    assert_visible(&fresh);
    assert_visible(&queried);
    assert!(
        !prompt(&fresh)["response_schema"]
            .to_string()
            .contains("\"const\":\"read_evidence\"")
    );
    let body = prompt(&queried);
    let read = body["response_schema"]["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["properties"]["type"]["const"] == "read_evidence")
        .unwrap();
    assert_eq!(read["properties"]["fact"]["enum"], json!([target.fact]));
    let mut evidence = Vec::new();
    for _ in 0..2 {
        acquire_source(
            &mut fixture,
            &target,
            &citation,
            &mut evidence,
            "ordinary source",
        );
        let read = build_source(&fixture, &raw, &evidence, ROLE, CompletionCallMode::Choice);
        assert_visible(&read);
        assert_eq!(prompt(&read)["turn_progress"]["phase"], "after_read");
    }
    for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
        let input = build_source(&fixture, &raw, &evidence, ROLE, mode);
        assert_visible(&input);
        let prepared = firewall
            .prepare_bounded(
                &provider,
                &input.action("visible:seal".into()).unwrap(),
                &limits,
            )
            .unwrap();
        input
            .seal_disclosure_with_policy(&prepared, policy)
            .unwrap();
        let reserve = input
            .measure_v9_reserve_with_policy(
                &provider.bounded_profile().unwrap(),
                &limits,
                "visible:reserve",
                policy,
            )
            .unwrap();
        assert!(reserve.choice.original_payload_bytes > reserve.finish_only.original_payload_bytes);
        for replacement in [
            None,
            Some(json!({})),
            Some(prompt(&fresh)["response_schema"].clone()),
        ] {
            let mut altered = input.clone();
            let mut body = prompt(&altered);
            if let Some(value) = replacement {
                body["response_schema"] = value;
            } else {
                body.as_object_mut().unwrap().remove("response_schema");
            }
            altered.payload.prompt = body.to_string();
            assert!(altered.admission_reserve_basis(policy).is_err());
            let prepared = firewall
                .prepare_bounded(
                    &provider,
                    &altered.action("visible:tampered".into()).unwrap(),
                    &limits,
                )
                .unwrap();
            assert!(
                altered
                    .seal_disclosure_with_policy(&prepared, policy)
                    .is_err()
            );
            assert!(
                sealing::admission_protected_fields(
                    &input.ledger,
                    &input.payload,
                    &altered.payload
                )
                .is_err()
            );
        }
        for schema in [
            format!(" {}", input.payload.response_schema.as_ref().unwrap()),
            "[".repeat(MAX_INPUT_BYTES + 1),
        ] {
            let mut altered = input.clone();
            altered.payload.response_schema = Some(schema);
            assert!(altered.admission_reserve_basis(policy).is_err());
        }
    }
}

#[test]
fn visible_schema_query_growth_uses_both_reserve_modes() {
    let policy = &INVESTIGATION_V2_POLICY;
    let (fixture, raw, _, _) = source_fixture(ROLE, "ordinary source");
    let initial_ledger = fixture.basis.previous_ledger.clone().unwrap();
    let basis = InvestigationTurnBasis {
        input_ledger_hash: initial_ledger.fingerprint_with_policy(policy).unwrap(),
        previous_ledger: None,
        completed_tool: None,
    };
    let before = InvestigationUsage::default();
    let initial = InvestigationInput::prepare_v9_with_policy(
        &fixture.request.question,
        ROLE,
        initial_ledger,
        &[],
        &[],
        &[],
        &before,
        &basis,
        &fixture.request,
        &fixture.metadata,
        &[],
        CompletionCallMode::Choice,
        0,
        None,
        policy,
    )
    .unwrap();
    let admitted = usage(CompletionCallMode::Choice);
    let mut after = admitted.clone();
    after.tool_actions = 1;
    after.selected_facts = fixture.ledger.selected_facts.len();
    let candidate = InvestigationInput::prepare_v9_with_policy(
        &fixture.request.question,
        ROLE,
        fixture.ledger.clone(),
        &[&raw],
        &[],
        &[],
        &after,
        &fixture.basis,
        &fixture.request,
        &fixture.metadata,
        &[],
        CompletionCallMode::Choice,
        0,
        None,
        policy,
    )
    .unwrap();
    assert_ne!(
        prompt(&initial)["response_schema"],
        prompt(&candidate)["response_schema"]
    );
    let mut repeated = fixture.ledger.clone();
    repeated.revision += 1;
    repeated.queries.push(repeated.queries[0].clone());
    let repeat_basis = InvestigationTurnBasis {
        input_ledger_hash: repeated.fingerprint_with_policy(policy).unwrap(),
        previous_ledger: Some(fixture.ledger.clone()),
        completed_tool: Some(InvestigationTool::QueryContext),
    };
    let repeat = InvestigationInput::prepare_v9_with_policy(
        &fixture.request.question,
        ROLE,
        repeated,
        &[&raw, &raw],
        &[],
        &[],
        &after,
        &repeat_basis,
        &fixture.request,
        &fixture.metadata,
        &[],
        CompletionCallMode::Choice,
        0,
        None,
        policy,
    )
    .unwrap();
    assert_visible(&repeat);
    assert_eq!(
        prompt(&repeat)["turn_progress"]["last_completed_action"]["no_progress"],
        true
    );
    assert_eq!(
        prompt(&repeat)["response_schema"],
        prompt(&candidate)["response_schema"]
    );
    let provider = OllamaProvider::local_default().unwrap();
    let profile = provider.bounded_profile().unwrap();
    let limits = CompletionLimits::default();
    let certified = initial
        .certify_context_preview_v9_with_policy(
            &candidate,
            ContextPreviewKind::Query,
            &raw,
            &admitted,
            &profile,
            &limits,
            "visible:next",
            policy,
        )
        .unwrap();
    assert!(
        certified
            .measurements()
            .continuation
            .choice
            .original_payload_bytes
            > certified
                .measurements()
                .continuation
                .finish_only
                .original_payload_bytes
    );
    let mut stale = candidate.clone();
    let mut body = prompt(&stale);
    body["response_schema"] = prompt(&initial)["response_schema"].clone();
    stale.payload.prompt = body.to_string();
    assert!(
        initial
            .certify_context_preview_v9_with_policy(
                &stale,
                ContextPreviewKind::Query,
                &raw,
                &admitted,
                &profile,
                &limits,
                "visible:next",
                policy
            )
            .is_err()
    );
}

#[test]
fn visible_schema_preserves_historical_wires_and_capacity() {
    const PROMPTS: [&str; 15] = [
        "ab952c9fe055e1fde7aa5ca3f1d7f60851b402021460fdbd9011a765e20967c3",
        "9eb83715f67f8b022ec5a144de89c4a4a86e0c3ee86ab71e545677e0fe3e2b37",
        "6525d4d18f258b3bbb12963e5e1fc5b26fbedb8bb30d25590a67bb9a539bd6e6",
        "503b9b28dd1b98cd660b17cdbd74016574cf445a33643b20ec71617956f17077",
        "ae67454605a84cc6af1cfd69b1ca7ccd8944ba3df46e02329583bc98412c3836",
        "235648d22c87b361dc32669531a0d4c00c1fc9e2843e172f08c60f91c42275e3",
        "0433cffbeab51580fcb7f31ee7c9be19aa8e38c8375a060d7f4abce9d3a9b3ac",
        "2d8fead7464741c8b030dcfc2187e012d6539c878f23912b58d53bb15f3b23c4",
        "2905d99715494afd020ae128c25ab1f6d1b039d923a557da44bc0987d393cda0",
        "e5be78acb8b65b9f1e8e69595570a47ceabfc83580aae08b35d087aa02187f54",
        "7130851c24b44cdf1083eafe74862ab5a6eebf9141f2a905d7e6387bec05ca35",
        "4cc0655d2e7954c4062c6d7c33cfef2f2d87ccc4a721398104a7afd7c675a4c8",
        "5ced835af727886fcae0d7ebd18348d97bea9a7a9dfdab9c164c8e6c1d7e9531",
        "b1634f6fb40d19c2ddc58fb8845e1f7f66bd6133d04c8a7e491df5910c857ffd",
        "ec99aee7d993cae30220094c8afb31430ba33a95b77cd4389ae81fbe0ef0041c",
    ];
    const WIRES: [&str; 6] = [
        "fa5284950ffeff00989878597025dbefd2582b8562487ce5ca2b69cf7ddf8fd0",
        "a78669d0ddbae89f404ff12f77cbf6162a5cfa73af5d9f697e25858c745533a5",
        "85ed7dd423683072531a0eca78b7526cd08906a52af2d498ed949da556083b63",
        "bd9f8e24fc580a06136582130cde1e0bc1243e97eb57d5cdf0c0baff4d64ba92",
        "56ea23174809d4e94072c31cb8351e8ad72d8d8183c39e1d8a0c00cfd84b1a38",
        "b9f997273103211919573fd3c3a9e0b46e3ab0b7f97f7b97d7fe28867cc17037",
    ];
    for version in 1..=15 {
        let role: SpecialistId =
            serde_json::from_value(json!(format!("domain-analyst@{version}"))).unwrap();
        assert_eq!(
            role.definition().unwrap().prompt_fingerprint,
            PROMPTS[version - 1]
        );
    }
    for version in 10..=15 {
        let role: SpecialistId =
            serde_json::from_value(json!(format!("domain-analyst@{version}"))).unwrap();
        let input = Fixture::new(role, 7)
            .build(
                role,
                &usage(CompletionCallMode::Choice),
                CompletionCallMode::Choice,
                0,
                None,
            )
            .unwrap();
        assert_eq!(
            content_hash(&serde_json::to_vec(&input.payload).unwrap()),
            WIRES[version - 10]
        );
        assert!(prompt(&input).get("response_schema").is_none());
        let mut injected = input.clone();
        let mut body = prompt(&injected);
        body["response_schema"] = json!({});
        injected.payload.prompt = body.to_string();
        assert!(
            injected
                .admission_reserve_basis(&INVESTIGATION_V2_POLICY)
                .is_err()
        );
    }
    let old = SpecialistId::DomainAnalystV15;
    let old = Fixture::new(old, 7)
        .build(
            old,
            &usage(CompletionCallMode::Choice),
            CompletionCallMode::Choice,
            0,
            None,
        )
        .unwrap();
    let visible = Fixture::new(ROLE, 7)
        .build(
            ROLE,
            &usage(CompletionCallMode::Choice),
            CompletionCallMode::Choice,
            0,
            None,
        )
        .unwrap();
    let provider = OllamaProvider::local_default().unwrap();
    let mut values = CompletionLimits::default().values();
    values.input_bytes = serde_json::to_vec(&old.payload).unwrap().len() as u64;
    let limits = CompletionLimits::new(values).unwrap();
    inspect_bounded_action(
        &provider.bounded_profile().unwrap(),
        &old.action("visible:boundary".into()).unwrap(),
        &limits,
    )
    .unwrap();
    assert_eq!(
        inspect_bounded_action(
            &provider.bounded_profile().unwrap(),
            &visible.action("visible:boundary".into()).unwrap(),
            &limits
        )
        .err()
        .unwrap()
        .code,
        FailureCode::InputTooLarge
    );
    assert_eq!(MAX_INPUT_BYTES, 128 * 1024);
    assert_eq!(prompt(&old)["limits"], prompt(&visible)["limits"]);
}
