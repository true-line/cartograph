//! AC-1330 (T-1330, #2482) and AC-1331 (T-1331, #2495): the rule-scope
//! check's request and reply.
use super::*;

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_owned()).collect()
}

fn rule<'a>(id: &'a str, text: &'a str, holds_for: &[&'a str]) -> ScopeRule<'a> {
    ScopeRule {
        id,
        text,
        holds_for: holds_for.to_vec(),
    }
}

#[test]
fn the_check_asks_one_answer_per_claim_and_rule_pair() {
    let first = ids(&["F-r1", "F-c1", "F-c2"]);
    let second = ids(&["F-r2", "F-r3"]);
    let claims = [
        ScopeClaim {
            statement: "Sign-in fails for every provider.",
            citations: &first,
            rules: vec![rule(
                "F-r1",
                "A Facebook error is shown",
                &["Facebook login"],
            )],
        },
        ScopeClaim {
            statement: "The API\nrefuses it.",
            citations: &second,
            rules: vec![
                rule("F-r2", "403 when not owned", &["Manage alias (API)"]),
                rule(
                    "F-r3",
                    "Empty body\nis 400",
                    &["Manage user (API)", "Manage auth (API)"],
                ),
            ],
        },
    ];
    let facts = BTreeMap::from([("F-c1".to_owned(), "x".repeat(FACT_TEXT_CHARS + 9))]);
    let action = action(&claims, &facts, "scope:1".into());
    let prompt = &action.payload.prompt;
    assert!(prompt.contains(&format!("F-c1: {}\n", "x".repeat(FACT_TEXT_CHARS))));
    assert!(prompt.contains("F-r3: (no text)"));
    assert!(prompt.contains(
        "1.1  Claim 1: \"Sign-in fails for every provider.\"  |  Rule F-r1 of \"Facebook login\": A Facebook error is shown\n"
    ));
    assert!(prompt.contains(
        "2.2  Claim 2: \"The API refuses it.\"  |  Rule F-r3 of \"Manage user (API)\", \"Manage auth (API)\": Empty body is 400\n"
    ));
    assert!(prompt.ends_with("3 in all."));
    let system = &action.payload.system;
    assert!(system.contains("Each rule holds only for the capabilities it is listed under"));
    assert!(system.contains("names other capabilities for other reasons in the same sentence"));
    let schema: serde_json::Value =
        serde_json::from_str(action.payload.response_schema.as_deref().unwrap()).unwrap();
    let answers = &schema["properties"]["answers"];
    assert_eq!(
        (answers["minItems"].clone(), answers["maxItems"].clone()),
        (3.into(), 3.into())
    );
    assert_eq!(
        answers["items"]["properties"]["pair"]["enum"],
        serde_json::json!(["1.1", "2.1", "2.2"])
    );
    assert_eq!(
        answers["items"]["properties"]["answer"]["enum"],
        serde_json::json!([WITHIN, WIDENED])
    );
}

#[test]
fn a_reply_must_answer_each_pair_once_and_tell_them_apart() {
    let cites = ids(&["F-r1", "F-r2"]);
    let claims = [ScopeClaim {
        statement: "s",
        citations: &cites,
        rules: vec![rule("F-r1", "a", &["A"]), rule("F-r2", "b", &["B"])],
    }];
    let good =
        r#"{"answers":[{"pair":"1.2","answer":"widened"},{"pair":"1.1","answer":"within"}]}"#;
    assert_eq!(decode(good, &claims), Some(vec![vec![false, true]]));
    for bad in [
        r#"{"answers":[{"pair":"1.1","answer":"within"}]}"#,
        r#"{"answers":[{"pair":"1.1","answer":"within"},{"pair":"1.1","answer":"within"}]}"#,
        r#"{"answers":[{"pair":"1.1","answer":"within"},{"pair":"1.3","answer":"within"}]}"#,
        r#"{"answers":[{"pair":"1.1","answer":"within"},{"pair":"1.2","answer":"maybe"}]}"#,
        // Every pair widened does not tell the pairs apart.
        r#"{"answers":[{"pair":"1.1","answer":"widened"},{"pair":"1.2","answer":"widened"}]}"#,
        r#"{"answers":[{"pair":"1.1","answer":"within","why":"x"},{"pair":"1.2","answer":"within"}]}"#,
        "not json",
    ] {
        assert!(decode(bad, &claims).is_none(), "{bad}");
    }
    // One pair may be widened on its own.
    let one = [ScopeClaim {
        statement: "s",
        citations: &cites,
        rules: vec![rule("F-r1", "a", &["A"])],
    }];
    let widened = r#"{"answers":[{"pair":"1.1","answer":"widened"}]}"#;
    assert_eq!(decode(widened, &one), Some(vec![vec![true]]));
}

#[test]
fn a_claim_is_left_out_when_most_readable_samples_widen_one_of_its_rules() {
    let sample = |first: bool, second: bool| Some(vec![vec![false, first], vec![second]]);
    // Claim 1's second rule is widened on two of three samples; claim 2's
    // rule on one.
    let samples = vec![
        sample(true, false),
        sample(true, true),
        sample(false, false),
    ];
    assert_eq!(widened_claims(&samples), Some(vec![true, false]));
    // An unreadable sample does not vote; one readable sample is no answer.
    assert_eq!(widened_claims(&[sample(true, true), None, None]), None);
    assert_eq!(
        widened_claims(&[sample(true, true), None, sample(false, true)]),
        Some(vec![false, true])
    );
}

#[test]
fn the_definition_fingerprint_covers_the_instructions() {
    assert!(definition_fingerprint().len() >= 16);
    assert_ne!(
        definition_fingerprint(),
        crate::claim_review::definition_fingerprint()
    );
}
