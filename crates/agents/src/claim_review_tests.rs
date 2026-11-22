//! AC-1325 (T-1325, #2419): the claim reviewer's request and reply.
use super::*;

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_owned()).collect()
}

#[test]
fn the_reviewer_reads_each_cited_fact_once_beside_the_claims() {
    let first = ids(&["F-a", "F-b"]);
    let second = ids(&["F-b", "F-c"]);
    let claims = [
        ReviewClaim {
            statement: "Every API call needs a key.",
            citations: &first,
        },
        ReviewClaim {
            statement: "Users\nlog in.",
            citations: &second,
        },
    ];
    let facts = BTreeMap::from([
        ("F-a".to_owned(), "Capability Manage user (API)".to_owned()),
        ("F-b".to_owned(), "x".repeat(FACT_TEXT_CHARS + 50)),
    ]);
    let action = action(&claims, &facts, "review:1".into());
    let prompt = &action.payload.prompt;
    assert_eq!(prompt.matches("F-b: ").count(), 1);
    assert!(prompt.contains(&format!("F-b: {}\n", "x".repeat(FACT_TEXT_CHARS))));
    assert!(prompt.contains("F-c: (no text)"));
    assert!(prompt.contains("1. Every API call needs a key. [cites: F-a, F-b]"));
    assert!(prompt.contains("2. Users log in. [cites: F-b, F-c]"));
    assert!(
        action
            .payload
            .system
            .contains("wider scope than the capability")
    );
    let schema: serde_json::Value =
        serde_json::from_str(action.payload.response_schema.as_deref().unwrap()).unwrap();
    assert_eq!(schema["properties"]["verdicts"]["minItems"], 2);
    assert_eq!(schema["properties"]["verdicts"]["maxItems"], 2);
    assert_eq!(definition_fingerprint(), definition_fingerprint());
}

#[test]
fn a_reply_gives_one_verdict_per_claim_or_none() {
    let reply =
        |verdicts: serde_json::Value| serde_json::json!({ "verdicts": verdicts }).to_string();
    let ok = reply(serde_json::json!([
        {"claim": 2, "verdict": "overreach"},
        {"claim": 1, "verdict": "shown"},
        {"claim": 3, "verdict": "partly"},
    ]));
    assert_eq!(
        decode(&ok, 3),
        Some(vec![Verdict::Shown, Verdict::Overreach, Verdict::Partly])
    );
    // A missing, repeated or out-of-range claim, or another shape.
    let missing = reply(serde_json::json!([{"claim": 1, "verdict": "shown"}]));
    assert_eq!(decode(&missing, 2), None);
    let twice = reply(serde_json::json!([
        {"claim": 1, "verdict": "shown"}, {"claim": 1, "verdict": "partly"},
    ]));
    assert_eq!(decode(&twice, 2), None);
    let out = reply(serde_json::json!([{"claim": 0, "verdict": "shown"}]));
    assert_eq!(decode(&out, 1), None);
    assert_eq!(decode("{\"claims\": []}", 1), None);
    assert_eq!(decode("not json", 1), None);
}
