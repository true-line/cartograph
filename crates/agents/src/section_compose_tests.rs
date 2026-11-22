//! AC-0862 (T-0862): the composer's request and reply.
use super::*;

fn address() -> SectionAddress {
    SectionAddress {
        catalogue_version: "1".into(),
        document: "D1".into(),
        section: "1.5".into(),
        scope: "system".into(),
    }
}

fn id(n: usize) -> String {
    format!("F-{n:016x}")
}

fn fact(n: usize) -> ComposeFact {
    ComposeFact {
        citation_id: id(n),
        label: "BusinessRule".into(),
        name: Some(format!("rule {n}")),
        tier: "Deterministic".into(),
        confidence: "Verified".into(),
    }
}

#[test]
fn the_request_carries_the_section_the_facts_and_a_closed_citation_schema() {
    let address = address();
    let facts = vec![fact(1), fact(2)];
    let groups = vec![ComposeGroup {
        citation_id: id(9),
        label: "Capability".into(),
        name: "Orders".into(),
        rests_on: vec![id(1)],
    }];
    let action = action(
        &ComposeInput {
            address: &address,
            title: "Business rules",
            section: "## 1.5 Business rules (P+A)\n\nrows\n",
            facts: &facts,
            groups: &groups,
        },
        "compose:1".into(),
    );
    let prompt = &action.payload.prompt;
    assert!(prompt.contains("## 1.5 Business rules (P+A)\n\nrows\n</section>"));
    assert!(prompt.contains(&format!(
        "{} | BusinessRule | rule 1 | Deterministic",
        id(1)
    )));
    assert!(prompt.contains(&format!(
        "Capability {} \"Orders\" rests on: {}",
        id(9),
        id(1)
    )));
    assert!(action.payload.spans.is_empty());
    // Section 1.5 is told how to read code-shaped rule rows; it comes first.
    let guide = prompt
        .find("<guidance>\nThis section lists rules as code")
        .unwrap();
    assert!(guide < prompt.find("<section>").unwrap());
    // 1.3 is grounded in its capabilities and their rules (#2375).
    let overview = guidance("1.3").unwrap();
    // AC-1323 (T-1323): operations in domain terms, no route or port (#2408).
    assert!(overview.contains("never spell out a route, URL path, HTTP method or port"));
    assert!(overview.contains("not use the words endpoint or route"));
    assert!(
        overview.contains("only by citing its rule")
            && overview.contains("a count only when a cited fact shows it")
    );
    // AC-1327 (T-1327): §1.1 is a short overview in domain terms (#2446).
    let purpose = guidance("1.1").unwrap();
    assert!(purpose.contains("six to ten claims, one per capability area or group of users"));
    assert!(purpose.contains("never name a route, URL path, HTTP method, port, callable or file"));
    assert!(
        purpose.contains("not use the words endpoint or route")
            && purpose.contains("a count only when a cited fact shows it")
    );
    // A stated route rests on a cited fact that shows it (#2328).
    assert!(SYSTEM.contains("a route you state must be one a fact you cite shows"));
    assert!(SYSTEM.contains("A number, count or version you state must appear in a fact you cite"));
    // Areas the section lists are followed, not reinvented.
    assert!(
        guidance("1.3")
            .unwrap()
            .contains("When the section lists capability areas")
    );
    // §1.1 is an overview of what it summarises, not a listing (#2319).
    assert!(
        guidance("1.1")
            .unwrap()
            .contains("not a capability-by-capability listing")
    );
    assert_eq!(guidance("9.9"), None);
    let schema: serde_json::Value =
        serde_json::from_str(action.payload.response_schema.as_deref().unwrap()).unwrap();
    let ids =
        &schema["properties"]["claims"]["items"]["properties"]["citation_ids"]["items"]["enum"];
    assert_eq!(ids, &serde_json::json!([id(1), id(2)]));
    // An uncited or empty reply is outside the schema, not only the decoder.
    assert_eq!(schema["properties"]["claims"]["minItems"], 1);
    assert_eq!(
        schema["properties"]["claims"]["items"]["properties"]["citation_ids"]["minItems"],
        1
    );
    // Hosted structured output can carry it.
    let text = action.payload.response_schema.unwrap();
    assert!(llm::bounded::anthropic_wire_schema(&text).is_some());
    assert!(llm::bounded::openai_cloud_wire_schema(&text).is_some());
    // The definition is stable and independent of the citable ids.
    assert_eq!(definition_fingerprint(), definition_fingerprint());
}

fn reply(claims: serde_json::Value) -> String {
    serde_json::json!({ "claims": claims }).to_string()
}

#[test]
fn a_reply_decodes_to_a_finish_and_out_of_bound_claims_are_left_out() {
    let citable: BTreeSet<String> = (1..=20).map(id).collect();
    let text = reply(serde_json::json!([
        {"statement": "Orders over the limit are refused with 400.",
         "claim_kind": "implemented_behavior", "citation_ids": [id(1), id(2), id(1)]},
        {"statement": "Cites an id that is not citable.",
         "claim_kind": "implemented_behavior", "citation_ids": [id(99)]},
        {"statement": "Cites too many.", "claim_kind": "inferred_interpretation",
         "citation_ids": (1..=17).map(id).collect::<Vec<_>>()},
        {"statement": "Cites nothing.", "claim_kind": "documented_intent", "citation_ids": []},
        {"statement": "A design.", "claim_kind": "proposed_design", "citation_ids": [id(3)]},
        {"statement": "Tests name the checkout\nas intended.",
         "claim_kind": "documented_intent", "citation_ids": [id(4)]},
        {"statement": " - ", "claim_kind": "implemented_behavior", "citation_ids": [id(5)]},
        {"statement": "x".repeat(2049), "claim_kind": "implemented_behavior",
         "citation_ids": [id(5)]},
        {"statement": "Bell \u{7} inside.", "claim_kind": "implemented_behavior",
         "citation_ids": [id(5)]},
    ]));
    let composed = decode(&address(), &text, &citable).unwrap();
    assert_eq!(composed.left_out, 7);
    // Each left-out claim is counted under the first bound it fails (#2349).
    assert_eq!(composed.reasons.values().sum::<usize>(), 7);
    assert_eq!(composed.reasons["citation_not_citable"], 3);
    assert_eq!(composed.reasons["claim_kind_unknown"], 1);
    assert_eq!(composed.finish.address, address());
    assert_eq!(
        composed.finish.claims,
        vec![
            Claim {
                statement: "Orders over the limit are refused with 400.".into(),
                kind: ClaimKind::Implemented,
                citations: vec![id(1), id(2)],
            },
            Claim {
                statement: "Tests name the checkout as intended.".into(),
                kind: ClaimKind::DocumentedIntent,
                citations: vec![id(4)],
            },
        ]
    );
}

#[test]
fn a_reply_that_is_not_the_schema_or_keeps_no_claim_is_refused() {
    let citable: BTreeSet<String> = [id(1)].into();
    for text in [
        "Here is the section: {}".to_owned(),
        r#"{"claims": [], "extra": 1}"#.to_owned(),
    ] {
        assert_eq!(
            decode(&address(), &text, &citable),
            Err(ComposeRefusal::NotTheSchema)
        );
    }
    let none = reply(serde_json::json!([
        {"statement": "x", "claim_kind": "implemented_behavior", "citation_ids": [id(2)]}
    ]));
    assert_eq!(
        decode(&address(), &none, &citable),
        Err(ComposeRefusal::NoClaims)
    );
    // A cited placeholder states nothing: it is not staged as a draft.
    let placeholder = reply(serde_json::json!([
        {"statement": "placeholder", "claim_kind": "implemented_behavior", "citation_ids": [id(1)]},
        {"statement": "To be determined later.", "claim_kind": "implemented_behavior",
         "citation_ids": [id(1)]}
    ]));
    assert_eq!(
        decode(&address(), &placeholder, &citable),
        Err(ComposeRefusal::NoClaims)
    );
}

#[test]
fn an_unlisted_schema_drops_the_id_list_and_decode_still_checks_ids() {
    // AC-1322 (T-1322, #2399).
    let address = address();
    let facts: Vec<ComposeFact> = (1..=40).map(fact).collect();
    let input = ComposeInput {
        address: &address,
        title: "Business rules",
        section: "## 1.5 Business rules (P+A)\n\nrows\n",
        facts: &facts,
        groups: &[],
    };
    let listed = action(&input, "compose:1".into());
    let unlisted = action_unlisted(&input, "compose:1".into());
    // The same request but for the schema.
    assert_eq!(listed.payload.prompt, unlisted.payload.prompt);
    assert_eq!(listed.payload.system, unlisted.payload.system);
    let schema = unlisted.payload.response_schema.as_deref().unwrap();
    assert!(!schema.contains(&id(1)));
    assert!(schema.len() + 40 * id(1).len() < listed.payload.response_schema.unwrap().len());
    let schema: serde_json::Value = serde_json::from_str(schema).unwrap();
    let claim = &schema["properties"]["claims"]["items"];
    assert_eq!(
        claim["properties"]["citation_ids"]["items"],
        serde_json::json!({"type": "string"})
    );
    assert_eq!(
        claim["required"],
        serde_json::json!(["statement", "claim_kind", "citation_ids"])
    );
    // An id the schema no longer excludes is still left out.
    let citable: BTreeSet<String> = facts.iter().map(|f| f.citation_id.clone()).collect();
    let text = reply(serde_json::json!([
        {"statement": "Orders over the limit are refused with 400.",
         "claim_kind": "implemented_behavior", "citation_ids": [id(1)]},
        {"statement": "Cites an id that is not citable.",
         "claim_kind": "implemented_behavior", "citation_ids": ["F-not-an-id"]},
    ]));
    let composed = decode(&address, &text, &citable).unwrap();
    assert_eq!(composed.finish.claims.len(), 1);
    assert_eq!(composed.reasons["citation_not_citable"], 1);
}

#[test]
fn a_cut_short_id_naming_one_citable_fact_is_read_as_that_fact() {
    // AC-1326 (T-1326, #2442).
    let citable: BTreeSet<String> = [
        "F-24b1ecd740c17dab",
        "F-778ec370eac8dd20",
        "F-778ec370eac8dd2f",
        "F-0000000000000001",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let reply = |ids: &[&str]| {
        serde_json::json!({"claims": [{
            "statement": "Scheduled jobs run on every host.",
            "claim_kind": "implemented_behavior",
            "citation_ids": ids,
        }]})
        .to_string()
    };
    // The last digit dropped: completed, and a repeat is folded once.
    let composed = decode(
        &address(),
        &reply(&["F-24b1ecd740c17da", "F-24b1ecd740c17dab"]),
        &citable,
    )
    .unwrap();
    assert_eq!(
        composed.finish.claims[0].citations,
        vec!["F-24b1ecd740c17dab".to_owned()]
    );
    // A prefix of two citable ids, one too short, a non-hex one, or one of
    // none stays as written and is left out.
    for id in [
        "F-778ec370eac8dd2",
        "F-24b1ecd740c17d",
        "F-24b1ecd740c17dz",
        "F-99b1ecd740c17da",
    ] {
        let text = reply(&[id, "F-0000000000000001"]);
        assert_eq!(
            decode(&address(), &text, &citable).err(),
            Some(ComposeRefusal::NoClaims),
            "{id}"
        );
    }
}

/// AC-1328 (T-1328): a composition asked once more is the same request
/// followed by the correction, under its own action id (#2449).
#[test]
fn a_corrected_request_is_the_same_request_and_the_correction() {
    let address = address();
    let facts = vec![fact(1)];
    let first = action(
        &ComposeInput {
            address: &address,
            title: "Business rules",
            section: "## 1.5 Business rules (P+A)\n\nrows\n",
            facts: &facts,
            groups: &[],
        },
        "compose:D1:1.3".into(),
    );
    let again = corrected(&first);
    assert_eq!(again.action_id, "compose:D1:1.3:corrected");
    assert_eq!(again.payload.system, first.payload.system);
    assert_eq!(again.payload.response_schema, first.payload.response_schema);
    assert_eq!(
        again.payload.prompt,
        format!(
            "{}\n<correction>\n{CORRECTION}\n</correction>\n",
            first.payload.prompt
        )
    );
    assert!(
        CORRECTION.contains("domain terms")
            && CORRECTION.contains("only when a fact you cite shows it")
    );
}
