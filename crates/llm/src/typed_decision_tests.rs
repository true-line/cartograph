//! AC-1518 (T-1518, #429): a typed decision leaves the machine only with
//! consent, and only a reply that fits its questions is returned.
use super::*;
use crate::{ConsentGrant, EgressFirewall, EgressPolicy};
use std::sync::Mutex;

/// A recorded provider: replays saved answers, counting every call.
struct Recorded {
    locality: Locality,
    answers: Vec<ChoiceAnswer>,
    seen: Mutex<Vec<DecisionRequest>>,
}
impl TypedDecisionProvider for Recorded {
    fn id(&self) -> &str {
        "recorded:jev-fixture"
    }
    fn locality(&self) -> Locality {
        self.locality
    }
    fn decide(&self, request: &DecisionRequest) -> Result<DecisionResponse, ProviderError> {
        self.seen.lock().unwrap().push(request.clone());
        Ok(DecisionResponse {
            answers: self.answers.clone(),
            reported_input_tokens: Some(42),
        })
    }
}

fn provider(locality: Locality, answers: Vec<ChoiceAnswer>) -> Recorded {
    Recorded {
        locality,
        answers,
        seen: Mutex::new(Vec::new()),
    }
}

fn request() -> DecisionRequest {
    DecisionRequest {
        state: "checkout calls charge(); api_key = \"sk-live-abcdefghijklmnopqrstuvwx\"".into(),
        questions: vec![ChoiceQuestion {
            id: "q1".into(),
            question: "Which candidate does the call resolve to?".into(),
            options: vec![
                "fn:pay.ts#charge".into(),
                "fn:refund.ts#charge".into(),
                "none".into(),
            ],
        }],
    }
}

fn answer(choice: usize, probabilities: Vec<f64>, confidence: f64) -> ChoiceAnswer {
    ChoiceAnswer {
        question_id: "q1".into(),
        choice,
        probabilities,
        confidence,
    }
}

fn good() -> ChoiceAnswer {
    answer(0, vec![0.8, 0.15, 0.05], 0.8)
}

#[test]
fn a_cloud_decision_without_policy_or_consent_never_reaches_the_provider() {
    let cloud = provider(Locality::Cloud, vec![good()]);
    let denied = EgressFirewall::new(EgressPolicy::local_only());
    assert!(matches!(
        denied.decide(&cloud, AnalysisTier::Semantic, "a1", &request(), None),
        Err(ProviderError::EgressDenied { .. })
    ));
    let allowed = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Semantic]));
    let Err(ProviderError::ConsentRequired(preview)) =
        allowed.decide(&cloud, AnalysisTier::Semantic, "a1", &request(), None)
    else {
        panic!("consent is required");
    };
    // The preview the user sees is already redacted.
    assert!(preview.redaction_count > 0);
    assert!(
        !preview
            .payload
            .prompt
            .contains("sk-live-abcdefghijklmnopqrstuvwx")
    );
    assert_eq!(preview.payload.system, DECISION_SYSTEM);
    // A grant for another action does not match.
    let mut other = ConsentGrant::from_preview(&preview);
    other.action_id = "a2".into();
    assert!(matches!(
        allowed.decide(
            &cloud,
            AnalysisTier::Semantic,
            "a1",
            &request(),
            Some(&other)
        ),
        Err(ProviderError::ConsentMismatch)
    ));
    assert!(cloud.seen.lock().unwrap().is_empty());
    let grant = ConsentGrant::from_preview(&preview);
    let response = allowed
        .decide(
            &cloud,
            AnalysisTier::Semantic,
            "a1",
            &request(),
            Some(&grant),
        )
        .unwrap();
    assert_eq!(response.answers, vec![good()]);
    let seen = cloud.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(!seen[0].state.contains("sk-live-abcdefghijklmnopqrstuvwx"));
}

#[test]
fn a_recorder_refusal_stops_the_call() {
    let cloud = provider(Locality::Cloud, vec![good()]);
    let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Semantic]))
        .with_egress_recorder(|_, _| false);
    let preview = firewall
        .preview_decision(&cloud, AnalysisTier::Semantic, "a1", &request())
        .unwrap();
    let grant = ConsentGrant::from_preview(&preview);
    assert!(matches!(
        firewall.decide(
            &cloud,
            AnalysisTier::Semantic,
            "a1",
            &request(),
            Some(&grant)
        ),
        Err(ProviderError::EgressUnrecorded { .. })
    ));
    assert!(cloud.seen.lock().unwrap().is_empty());
}

#[test]
fn only_a_reply_that_fits_its_questions_is_returned() {
    let firewall = EgressFirewall::new(EgressPolicy::local_only());
    for bad in [
        vec![],
        vec![good(), good()],
        vec![answer(3, vec![0.8, 0.15, 0.05], 0.8)],
        vec![answer(0, vec![0.8, 0.2], 0.8)],
        vec![answer(0, vec![0.8, 0.8, 0.05], 0.8)],
        vec![answer(0, vec![0.8, 0.15, 0.05], 1.5)],
        vec![answer(0, vec![f64::NAN, 0.15, 0.05], 0.8)],
        vec![ChoiceAnswer {
            question_id: "q9".into(),
            ..good()
        }],
    ] {
        let local = provider(Locality::Local, bad.clone());
        assert!(
            matches!(
                firewall.decide(&local, AnalysisTier::Semantic, "a1", &request(), None),
                Err(ProviderError::InvalidResponse(_))
            ),
            "{bad:?}"
        );
    }
    let local = provider(Locality::Local, vec![good()]);
    assert!(
        firewall
            .decide(&local, AnalysisTier::Semantic, "a1", &request(), None)
            .is_ok()
    );
}

#[test]
fn a_request_needs_distinct_questions_with_two_to_255_options() {
    let firewall = EgressFirewall::new(EgressPolicy::local_only());
    let local = provider(Locality::Local, vec![good()]);
    let mut one = request();
    one.questions[0].options.truncate(1);
    let mut many = request();
    many.questions[0].options = (0..256).map(|n| n.to_string()).collect();
    let mut twice = request();
    twice.questions.push(twice.questions[0].clone());
    let mut none = request();
    none.questions.clear();
    for bad in [one, many, twice, none] {
        assert!(
            firewall
                .preview_decision(&local, AnalysisTier::Semantic, "a1", &bad)
                .is_err()
        );
    }
    assert!(local.seen.lock().unwrap().is_empty());
}

#[test]
fn every_request_string_is_redacted_before_it_is_encoded() {
    // JSON escapes the quotes, so redacting the encoded request would miss
    // a quoted credential assignment; each string is redacted first.
    let secret = "hunter2hunter2";
    let request = DecisionRequest {
        state: format!("login() checks password=\"{secret}\""),
        questions: vec![ChoiceQuestion {
            id: "q1".into(),
            question: format!("Which verify runs for password=\"{secret}\"?"),
            options: vec![
                "fn:auth/jwt.ts#verify".into(),
                format!("password=\"{secret}\""),
                "none".into(),
            ],
        }],
    };
    let cloud = provider(Locality::Cloud, vec![good()]);
    let allowed = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Semantic]));
    let preview = allowed
        .preview_decision(&cloud, AnalysisTier::Semantic, "a1", &request)
        .unwrap();
    assert!(
        !preview.payload.prompt.contains(secret),
        "{}",
        preview.payload.prompt
    );
    assert!(preview.redaction_count >= 3);
    let grant = ConsentGrant::from_preview(&preview);
    allowed
        .decide(&cloud, AnalysisTier::Semantic, "a1", &request, Some(&grant))
        .unwrap();
    let seen = cloud.seen.lock().unwrap();
    assert!(!format!("{:?}", seen[0]).contains(secret));
}
