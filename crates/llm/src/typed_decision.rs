//! Consented typed-decision providers (#429, ADR-0090).
//!
//! A typed-decision provider returns no prose. It takes a state blob and
//! choice questions, and answers each one with a choice, a probability per
//! option and a confidence (the shape of TypeSafe Jev). Completion providers
//! go through [`LlmProvider`](crate::LlmProvider); this trait is narrower and
//! reuses the same firewall: the payload is redacted and previewed, a cloud
//! call needs the tier allowed by policy and a grant for that exact preview,
//! and a denied or ungranted call never reaches the provider.
//!
//! An answer is inferred, never confirmed: callers record it at the tier
//! ADR-0090 names, with the answer's own confidence.

use crate::{
    AnalysisTier, CompletionPayload, ConsentGrant, EgressFirewall, EgressPreview, Locality,
    ProviderError,
};
use serde::{Deserialize, Serialize};

/// The system line of every typed-decision payload, so a completion grant
/// never matches a decision preview.
pub const DECISION_SYSTEM: &str = "typed-decision@1";

/// At most this many options in one question.
pub const MAX_OPTIONS: usize = 255;

/// How far the probabilities of one answer may sum from 1.
const PROBABILITY_TOLERANCE: f64 = 1e-3;

/// One choice question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceQuestion {
    /// Stable id the answer names.
    pub id: String,
    /// The question.
    pub question: String,
    /// The options, at least two and at most [`MAX_OPTIONS`].
    pub options: Vec<String>,
}

/// The request: the state the questions are about, and the questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    /// The state blob, redacted before it leaves the machine.
    pub state: String,
    /// The questions, each id once.
    pub questions: Vec<ChoiceQuestion>,
}

/// One answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceAnswer {
    /// The question's id.
    pub question_id: String,
    /// The chosen option's index.
    pub choice: usize,
    /// One probability per option, summing to 1.
    pub probabilities: Vec<f64>,
    /// The provider's confidence in the choice, from 0 to 1.
    pub confidence: f64,
}

/// The provider's reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionResponse {
    /// One answer per question, in any order.
    pub answers: Vec<ChoiceAnswer>,
    /// Input tokens the provider reported, if any.
    pub reported_input_tokens: Option<u64>,
}

/// A provider of typed decisions.
pub trait TypedDecisionProvider: Send + Sync {
    /// Stable provider and model id.
    fn id(&self) -> &str;
    /// Where the provider runs.
    fn locality(&self) -> Locality;
    /// Answer the redacted request. Called only through
    /// [`EgressFirewall::decide`].
    fn decide(&self, request: &DecisionRequest) -> Result<DecisionResponse, ProviderError>;
}

fn payload(request: &DecisionRequest) -> Result<CompletionPayload, ProviderError> {
    Ok(CompletionPayload {
        system: DECISION_SYSTEM.to_owned(),
        prompt: serde_json::to_string(request)?,
        spans: Vec::new(),
        response_schema: None,
    })
}

fn validate_request(request: &DecisionRequest) -> Result<(), ProviderError> {
    let mut ids = std::collections::BTreeSet::new();
    let shaped = !request.questions.is_empty()
        && request.questions.iter().all(|question| {
            ids.insert(question.id.as_str()) && (2..=MAX_OPTIONS).contains(&question.options.len())
        });
    if shaped {
        Ok(())
    } else {
        Err(ProviderError::Unsupported("decision request shape"))
    }
}

/// Check that `response` answers each question of `request` once, with a
/// choice among its options, one finite probability per option summing to 1
/// and a confidence between 0 and 1.
pub fn validate_response(
    request: &DecisionRequest,
    response: &DecisionResponse,
) -> Result<(), ProviderError> {
    let invalid = |why: &str| Err(ProviderError::InvalidResponse(why.to_owned()));
    if response.answers.len() != request.questions.len() {
        return invalid("one answer per question");
    }
    let mut seen = std::collections::BTreeSet::new();
    for answer in &response.answers {
        let Some(question) = request
            .questions
            .iter()
            .find(|question| question.id == answer.question_id)
        else {
            return invalid("an answer names no question");
        };
        if !seen.insert(answer.question_id.as_str()) {
            return invalid("a question is answered twice");
        }
        let probabilities = &answer.probabilities;
        if answer.choice >= question.options.len()
            || probabilities.len() != question.options.len()
            || probabilities
                .iter()
                .any(|p| !p.is_finite() || !(0.0..=1.0).contains(p))
            || (probabilities.iter().sum::<f64>() - 1.0).abs() > PROBABILITY_TOLERANCE
            || !answer.confidence.is_finite()
            || !(0.0..=1.0).contains(&answer.confidence)
        {
            return invalid("an answer is outside its question's options or ranges");
        }
    }
    Ok(())
}

impl EgressFirewall {
    /// The exact redacted preview of a decision request, without calling
    /// the provider. A cloud provider needs `tier` allowed by policy.
    pub fn preview_decision(
        &self,
        provider: &dyn TypedDecisionProvider,
        tier: AnalysisTier,
        action_id: &str,
        request: &DecisionRequest,
    ) -> Result<EgressPreview, ProviderError> {
        validate_request(request)?;
        if provider.locality() == Locality::Cloud && !self.policy.cloud_allowed(tier) {
            return Err(ProviderError::EgressDenied {
                provider: provider.id().to_string(),
                tier,
            });
        }
        let (payload, redaction_count) = crate::redact_payload(&payload(request)?);
        let hash_bytes = serde_json::to_vec(&(
            provider.id(),
            provider.locality(),
            tier,
            action_id,
            &payload,
        ))?;
        Ok(EgressPreview {
            provider_id: provider.id().to_string(),
            locality: provider.locality(),
            tier,
            action_id: action_id.to_owned(),
            payload,
            payload_hash: blake3::hash(&hash_bytes).to_hex().to_string(),
            redaction_count,
        })
    }

    /// Ask a local provider at once, or a cloud provider only with a grant
    /// for this exact preview. The provider sees the redacted request only,
    /// and its reply is checked against the request.
    pub fn decide(
        &self,
        provider: &dyn TypedDecisionProvider,
        tier: AnalysisTier,
        action_id: &str,
        request: &DecisionRequest,
        consent: Option<&ConsentGrant>,
    ) -> Result<DecisionResponse, ProviderError> {
        let preview = self.preview_decision(provider, tier, action_id, request)?;
        if preview.locality == Locality::Cloud {
            let Some(grant) = consent else {
                return Err(ProviderError::ConsentRequired(Box::new(preview)));
            };
            if !grant.matches(&preview) {
                return Err(ProviderError::ConsentMismatch);
            }
            if let Some(recorder) = &self.recorder {
                let bytes = serde_json::to_vec(&preview.payload)?.len() as u64;
                if !recorder(&preview.provider_id, bytes) {
                    return Err(ProviderError::EgressUnrecorded {
                        provider: preview.provider_id,
                    });
                }
            }
        }
        let redacted: DecisionRequest = serde_json::from_str(&preview.payload.prompt)?;
        let response = provider.decide(&redacted)?;
        validate_response(&redacted, &response)?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "typed_decision_tests.rs"]
mod tests;
