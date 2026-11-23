//! AC-1330 (T-1330, #2482): the rule-scope check's request and reply.
use super::*;

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_owned()).collect()
}

fn claim<'a>(
    statement: &'a str,
    citations: &'a [String],
    rules: &[&'a str],
    caps: &[&'a str],
) -> ScopeClaim<'a> {
    ScopeClaim {
        statement,
        citations,
        rules: rules.to_vec(),
        capabilities: caps.to_vec(),
    }
}

#[test]
fn the_check_asks_one_scope_per_cited_rule_from_the_claims_own_capabilities() {
    let first = ids(&["F-r1", "F-c1", "F-c2"]);
    let second = ids(&["F-r2", "F-r3", "F-c3"]);
    let claims = [
        claim(
            "Sign-in fails for every provider.",
            &first,
            &["F-r1"],
            &["F-c1", "F-c2"],
        ),
        claim(
            "The API\nrefuses it.",
            &second,
            &["F-r2", "F-r3"],
            &["F-c3"],
        ),
    ];
    let facts = BTreeMap::from([
        (
            "F-r1".to_owned(),
            "Rule of capability \"Facebook login\": errors".to_owned(),
        ),
        ("F-c1".to_owned(), "x".repeat(FACT_TEXT_CHARS + 9)),
    ]);
    let action = action(&claims, &facts, "scope:1".into());
    let prompt = &action.payload.prompt;
    assert!(prompt.contains("F-r1: Rule of capability \"Facebook login\": errors\n"));
    assert!(prompt.contains(&format!("F-c1: {}\n", "x".repeat(FACT_TEXT_CHARS))));
    assert!(prompt.contains("F-r3: (no text)"));
    assert!(
        prompt.contains(
            "1. Sign-in fails for every provider. [cites: F-r1, F-c1, F-c2] [rules: F-r1]"
        )
    );
    assert!(
        prompt.contains("2. The API refuses it. [cites: F-r2, F-r3, F-c3] [rules: F-r2, F-r3]")
    );
    assert!(prompt.ends_with("3 in all."));
    assert!(
        action
            .payload
            .system
            .contains("not every capability named in the same sentence")
    );
    assert!(action.payload.system.contains("never what the rule says"));
    let schema: serde_json::Value =
        serde_json::from_str(action.payload.response_schema.as_deref().unwrap()).unwrap();
    let scopes = &schema["properties"]["scopes"];
    assert_eq!(scopes["minItems"], 3);
    assert_eq!(scopes["maxItems"], 3);
    let second_branch = &scopes["items"]["anyOf"][1]["properties"];
    assert_eq!(second_branch["claim"]["enum"], serde_json::json!([2]));
    assert_eq!(
        second_branch["rule"]["enum"],
        serde_json::json!(["F-r2", "F-r3"])
    );
    assert_eq!(
        second_branch["applies_to"]["items"]["enum"],
        serde_json::json!(["F-c3", ALL])
    );
}

#[test]
fn a_reply_must_give_each_rule_of_each_claim_exactly_once() {
    let first = ids(&["F-r1", "F-c1"]);
    let claims = [claim("s", &first, &["F-r1"], &["F-c1"])];
    let good = r#"{"scopes":[{"claim":1,"rule":"F-r1","applies_to":["F-c1"]}]}"#;
    let decoded = decode(good, &claims).unwrap();
    assert_eq!(decoded[0]["F-r1"], vec!["F-c1".to_owned()]);
    for bad in [
        r#"{"scopes":[]}"#,
        r#"{"scopes":[{"claim":1,"rule":"F-r1","applies_to":["F-c1"]},{"claim":1,"rule":"F-r1","applies_to":["all"]}]}"#,
        r#"{"scopes":[{"claim":2,"rule":"F-r1","applies_to":["F-c1"]}]}"#,
        r#"{"scopes":[{"claim":0,"rule":"F-r1","applies_to":["F-c1"]}]}"#,
        r#"{"scopes":[{"claim":1,"rule":"F-x","applies_to":["F-c1"]}]}"#,
        r#"{"scopes":[{"claim":1,"rule":"F-r1","applies_to":["F-c9"]}]}"#,
        r#"{"scopes":[{"claim":1,"rule":"F-r1","applies_to":[]}]}"#,
        r#"{"scopes":[{"claim":1,"rule":"F-r1","applies_to":["F-c1"],"note":"x"}]}"#,
        "not json",
    ] {
        assert!(decode(bad, &claims).is_none(), "{bad}");
    }
}

#[test]
fn a_claim_is_left_out_when_most_readable_samples_widen_one_of_its_rules() {
    let holds = |caps: &[&'static str]| caps.iter().copied().collect::<BTreeSet<&str>>();
    let in_scope = vec![
        BTreeMap::from([("F-r1", holds(&["F-c1"]))]),
        BTreeMap::from([("F-r2", holds(&["F-c2", "F-c3"]))]),
    ];
    let scope = |first: &[&str], second: &[&str]| {
        Some(vec![
            BTreeMap::from([("F-r1".to_owned(), ids(first))]),
            BTreeMap::from([("F-r2".to_owned(), ids(second))]),
        ])
    };
    // Claim 1 states its rule for all on two of three samples; claim 2
    // names an identical rule's capability (in scope) and once one outside.
    let samples = vec![
        scope(&[ALL], &["F-c2", "F-c3"]),
        scope(&["F-c1", "F-c9"], &["F-c3"]),
        scope(&["F-c1"], &["F-c4"]),
    ];
    assert_eq!(widened_claims(&samples, &in_scope), Some(vec![true, false]));
    // An unreadable sample does not vote; one readable sample is no answer.
    let one = vec![scope(&[ALL], &[ALL]), None, None];
    assert_eq!(widened_claims(&one, &in_scope), None);
    let two = vec![scope(&[ALL], &["F-c2"]), None, scope(&["F-c1"], &["F-c2"])];
    assert_eq!(widened_claims(&two, &in_scope), Some(vec![false, false]));
}

#[test]
fn the_definition_fingerprint_covers_the_instructions() {
    assert!(definition_fingerprint().len() >= 16);
    assert_ne!(
        definition_fingerprint(),
        crate::claim_review::definition_fingerprint()
    );
}
