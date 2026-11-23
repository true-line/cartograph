//! AC-1518 (T-1518, #429): the typed-decision eval harness on a recorded
//! fixture, with consent enforced on every case.
use super::*;
use llm::typed_decision::{ChoiceAnswer, DecisionResponse};
use llm::{EgressPolicy, Locality};
use serde_json::Value;
use std::sync::Mutex;

fn fixture() -> (Vec<DecisionCase>, Value) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/decision-recorded-v1/cases.json");
    let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let cases = serde_json::from_value(value["cases"].clone()).unwrap();
    (cases, value["recorded_answers"].clone())
}

/// Replays the recorded answers as a cloud provider would, counting calls
/// and keeping the requests it received.
struct Replay {
    answers: Value,
    seen: Mutex<Vec<DecisionRequest>>,
}
impl TypedDecisionProvider for Replay {
    fn id(&self) -> &str {
        "recorded:decision-recorded-v1"
    }
    fn locality(&self) -> Locality {
        Locality::Cloud
    }
    fn decide(&self, request: &DecisionRequest) -> Result<DecisionResponse, ProviderError> {
        self.seen.lock().unwrap().push(request.clone());
        let question = &request.questions[0];
        let recorded = &self.answers[&question.id];
        Ok(DecisionResponse {
            answers: vec![ChoiceAnswer {
                question_id: question.id.clone(),
                choice: serde_json::from_value(recorded["choice"].clone()).unwrap(),
                probabilities: serde_json::from_value(recorded["probabilities"].clone()).unwrap(),
                confidence: serde_json::from_value(recorded["confidence"].clone()).unwrap(),
            }],
            reported_input_tokens: Some(100),
        })
    }
}

fn replay() -> Replay {
    Replay {
        answers: fixture().1,
        seen: Mutex::new(Vec::new()),
    }
}

fn allowed() -> EgressFirewall {
    EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Semantic]))
}

#[test]
fn every_case_needs_its_own_consent_and_none_leaves_without_it() {
    let (cases, _) = fixture();
    let provider = replay();
    let denied = run(
        &EgressFirewall::new(EgressPolicy::local_only()),
        &provider,
        &cases,
        |_| panic!("a denied tier shows no preview"),
    )
    .unwrap();
    assert!(denied.iter().all(|o| o.refused.is_some() && !o.correct));
    let declined = run(&allowed(), &provider, &cases, |_| None).unwrap();
    assert!(
        declined
            .iter()
            .all(|o| o.refused.is_some() && o.payload_bytes > 0)
    );
    assert!(provider.seen.lock().unwrap().is_empty());
    // Consent for one case only: one call leaves.
    let one = run(&allowed(), &provider, &cases, |preview| {
        (preview.action_id == "decision-eval:d-retry").then(|| ConsentGrant::from_preview(preview))
    })
    .unwrap();
    assert_eq!(one.iter().filter(|o| o.refused.is_none()).count(), 1);
    assert_eq!(provider.seen.lock().unwrap().len(), 1);
}

#[test]
fn the_report_scores_choices_calibration_latency_and_cost() {
    let (cases, _) = fixture();
    let provider = replay();
    let outcomes = run(&allowed(), &provider, &cases, |preview| {
        Some(ConsentGrant::from_preview(preview))
    })
    .unwrap();
    // The secret in d-token's state never reached the provider.
    let seen = provider.seen.lock().unwrap();
    assert_eq!(seen.len(), cases.len());
    assert!(
        seen.iter()
            .all(|r| !r.state.contains("sk-live-abcdefghijklmnopqrstuvwx"))
    );
    // Every request offers the candidates plus an explicit none.
    assert!(seen.iter().zip(&cases).all(|(r, c)| {
        r.questions[0].options.len() == c.candidates.len() + 1
            && r.questions[0].options.last().map(String::as_str) == Some(NONE_OPTION)
    }));
    drop(seen);
    let report = report(provider.id(), outcomes, 0.042, Some(0.5));
    assert_eq!((report.answered, report.refused), (6, 0));
    // d-audit picks the null log: 5 of 6 correct. d-format's re-export and
    // d-external's none are both acceptable.
    assert_eq!(report.precision_at_1, Some(5.0 / 6.0));
    let wrong: Vec<&str> = report
        .outcomes
        .iter()
        .filter(|o| !o.correct)
        .map(|o| o.case_id.as_str())
        .collect();
    assert_eq!(wrong, ["d-audit"]);
    // Bins: 0.5 holds 0.55 (right) and 0.58 (wrong); 0.7; 0.9 holds 0.92,
    // 0.97 and 0.9.
    let counts: Vec<(usize, usize)> = report
        .calibration
        .iter()
        .map(|b| ((b.lower * 10.0).round() as usize, b.count))
        .collect();
    assert_eq!(counts, [(5, 2), (7, 1), (9, 3)]);
    let ece = report.expected_calibration_error.unwrap();
    let expected = (2.0 / 6.0) * (0.5f64 - 0.565).abs()
        + (1.0 / 6.0) * (1.0f64 - 0.7).abs()
        + (3.0 / 6.0) * (1.0f64 - (0.92 + 0.97 + 0.9) / 3.0).abs();
    assert!((ece - expected).abs() < 1e-9, "{ece} vs {expected}");
    assert!(report.latency_p50_ms.is_some() && report.latency_p95_ms >= report.latency_p50_ms);
    assert_eq!(report.input_tokens, Some(600));
    assert!((report.input_cost_usd.unwrap() - 600.0 / 1e6 * 0.042).abs() < 1e-12);
    assert_eq!(report.local_precision_at_1, Some(0.5));
}

#[test]
fn a_decision_is_never_recorded_above_the_t3_ceiling() {
    assert_eq!(DECISION_CONFIDENCE, ConfidenceTier::InferredWeak);
    assert_eq!(
        DECISION_CONFIDENCE,
        core_prov::Tier::Agentic.confidence_ceiling()
    );
}

/// Embeds by shared words, so the local resolver's choice is predictable.
struct Words;
impl LlmProvider for Words {
    fn id(&self) -> &str {
        "test-words"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> llm::ProviderCaps {
        llm::ProviderCaps {
            embeddings: true,
            chat: false,
            tool_use: false,
        }
    }
    fn embed(&self, batch: &[String]) -> Result<Vec<llm::Embedding>, ProviderError> {
        Ok(batch
            .iter()
            .map(|text| {
                ["charge", "payments", "refunds", "retry", "util", "queue"]
                    .iter()
                    .map(|word| f32::from(text.contains(word)))
                    .chain([0.1])
                    .collect()
            })
            .collect())
    }
}

#[test]
fn the_local_resolver_is_scored_on_the_same_cases() {
    let case = |id: &str, state: &str, candidates: &[&str], acceptable: &[&str]| DecisionCase {
        case_id: id.into(),
        state: state.into(),
        candidates: candidates.iter().map(|c| (*c).to_owned()).collect(),
        acceptable: acceptable.iter().map(|c| (*c).to_owned()).collect(),
    };
    let cases = [
        // Nearest is the payments charge: right.
        case(
            "a",
            "calls payments charge",
            &["fn:payments#charge", "fn:refunds#charge"],
            &["fn:payments#charge"],
        ),
        // Nearest is the util retry, but the label says the queue: wrong.
        case(
            "b",
            "calls util retry",
            &["fn:util#retry", "fn:queue#retry"],
            &["fn:queue#retry"],
        ),
    ];
    assert_eq!(
        local_precision_at_1(&Words, &cases, 0.0).unwrap(),
        Some(0.5)
    );
    // Above every similarity, the resolver answers none, which no case accepts.
    assert_eq!(
        local_precision_at_1(&Words, &cases, 1.01).unwrap(),
        Some(0.0)
    );
    assert_eq!(local_precision_at_1(&Words, &[], 0.0).unwrap(), None);
}
