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
    let system = &action.payload.system;
    assert!(system.contains("wider scope than the capability"));
    // AC-1329 (T-1329): everyday operations on what a cited capability
    // manages are a detail; a caller group, platform or job no cited fact
    // names is an overreach (#2458).
    assert!(
        system.contains("everyday operations (viewing, listing, creating, changing, removing)")
    );
    assert!(system.contains("a caller group, platform or component no cited fact names"));
    assert!(system.contains("beyond those everyday operations"));
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

/// AC-1329 (T-1329): a claim is left out when more than half of at least two
/// readable samples judge it an overreach (#2458).
#[test]
fn a_claim_is_left_out_on_a_majority_of_samples() {
    use Verdict::{Overreach as O, Partly as P, Shown as S};
    let samples = vec![vec![O, S, O], vec![O, O, P], vec![S, O, P]];
    assert_eq!(overreaching(&samples, 3), Some(vec![true, true, false]));
    // An unreadable sample does not vote; two readable ones must agree.
    let two = vec![vec![O, O, S], vec![O, S, S], Vec::new()];
    assert_eq!(overreaching(&two, 3), Some(vec![true, false, false]));
    assert_eq!(overreaching(&[vec![O, O, O]], 3), None);
    assert_eq!(overreaching(&[vec![O], vec![O]], 3), None);
    assert_eq!(REVIEW_SAMPLES, 3);
}
