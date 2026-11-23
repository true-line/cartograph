//! Typed-decision eval harness for candidate selection (#429, ADR-0091).
//!
//! Each case is one unresolved hop: its cited context (the state), the
//! resolved candidates, and its label (the acceptable targets, or no match).
//! The provider answers one choice question per case over the candidates plus
//! an explicit `none` option, so "no acceptable target" stays expressible.
//! Every call goes through [`EgressFirewall::decide`]: a case whose consent
//! is absent or refused is recorded as refused and never reaches the
//! provider. The report gives precision@1, a calibration table and its
//! expected calibration error, p50/p95 latency and input cost, beside the
//! local embedding resolver's top-1 on the same cases.
//!
//! An answer is a proposal at [`DECISION_CONFIDENCE`] with its returned
//! confidence; it never produces, overwrites or upgrades a T0/T1 fact.

use crate::SemanticError;
use core_prov::ConfidenceTier;
use llm::typed_decision::{ChoiceQuestion, DecisionRequest, TypedDecisionProvider};
use llm::{AnalysisTier, ConsentGrant, EgressFirewall, EgressPreview, LlmProvider, ProviderError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::Instant;

/// The confidence every typed-decision answer is recorded at (ADR-0091):
/// the T3 ceiling, never above it.
pub const DECISION_CONFIDENCE: ConfidenceTier = ConfidenceTier::InferredWeak;

/// The option that answers "no acceptable candidate".
pub const NONE_OPTION: &str = "none";

/// Calibration bins over [0, 1].
pub const CALIBRATION_BINS: usize = 10;

/// One labelled case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionCase {
    /// Stable case id.
    pub case_id: String,
    /// The hop's cited context.
    pub state: String,
    /// Resolved candidate ids, in a fixed order.
    pub candidates: Vec<String>,
    /// The acceptable targets; empty means no candidate is acceptable.
    pub acceptable: BTreeSet<String>,
}

/// What happened on one case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionOutcome {
    /// The case.
    pub case_id: String,
    /// The chosen candidate, `None` for the `none` option or no answer.
    pub chosen: Option<String>,
    /// The answer's confidence; `None` when the call did not run.
    pub confidence: Option<f64>,
    /// Whether the choice is acceptable (`none` is right for a no-match case).
    pub correct: bool,
    /// Why the call did not run, if it did not.
    pub refused: Option<String>,
    /// Wall time of the call in milliseconds.
    pub latency_ms: Option<f64>,
    /// Bytes of the redacted payload.
    pub payload_bytes: usize,
    /// Input tokens the provider reported.
    pub reported_input_tokens: Option<u64>,
}

/// One calibration bin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationBin {
    /// Lower confidence bound (inclusive).
    pub lower: f64,
    /// Upper confidence bound (inclusive for the last bin).
    pub upper: f64,
    /// Answers in the bin.
    pub count: usize,
    /// Mean confidence of those answers.
    pub mean_confidence: f64,
    /// Share of them that were correct.
    pub accuracy: f64,
}

/// The report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionReport {
    /// Provider id.
    pub provider_id: String,
    /// Per-case outcomes.
    pub outcomes: Vec<DecisionOutcome>,
    /// Cases answered.
    pub answered: usize,
    /// Cases refused before any call.
    pub refused: usize,
    /// Correct answers over answered cases.
    pub precision_at_1: Option<f64>,
    /// Calibration table over answered cases.
    pub calibration: Vec<CalibrationBin>,
    /// Expected calibration error over answered cases.
    pub expected_calibration_error: Option<f64>,
    /// Median latency in milliseconds.
    pub latency_p50_ms: Option<f64>,
    /// 95th percentile latency in milliseconds.
    pub latency_p95_ms: Option<f64>,
    /// Sum of reported input tokens, when every answer reported them.
    pub input_tokens: Option<u64>,
    /// Input cost at `price_per_million_input_tokens`, when tokens are known.
    pub input_cost_usd: Option<f64>,
    /// The local resolver's top-1 precision on the same cases, if run.
    pub local_precision_at_1: Option<f64>,
}

fn request(case: &DecisionCase) -> DecisionRequest {
    let mut options = case.candidates.clone();
    options.push(NONE_OPTION.to_owned());
    DecisionRequest {
        state: case.state.clone(),
        questions: vec![ChoiceQuestion {
            id: case.case_id.clone(),
            question: "Which candidate does this hop resolve to? Answer none when no candidate \
                       fits."
                .to_owned(),
            options,
        }],
    }
}

fn scored(case: &DecisionCase, chosen: Option<&String>) -> bool {
    match chosen {
        Some(id) => case.acceptable.contains(id),
        None => case.acceptable.is_empty(),
    }
}

/// Run every case through the firewall. `consent` sees each exact redacted
/// preview and returns a grant for it, or `None` to refuse that call.
pub fn run(
    firewall: &EgressFirewall,
    provider: &dyn TypedDecisionProvider,
    cases: &[DecisionCase],
    mut consent: impl FnMut(&EgressPreview) -> Option<ConsentGrant>,
) -> Result<Vec<DecisionOutcome>, SemanticError> {
    let tier = AnalysisTier::Semantic;
    let mut outcomes = Vec::with_capacity(cases.len());
    for case in cases {
        let request = request(case);
        let action_id = format!("decision-eval:{}", case.case_id);
        let refused = |why: String, payload_bytes| DecisionOutcome {
            case_id: case.case_id.clone(),
            chosen: None,
            confidence: None,
            correct: false,
            refused: Some(why),
            latency_ms: None,
            payload_bytes,
            reported_input_tokens: None,
        };
        let preview = match firewall.preview_decision(provider, tier, &action_id, &request) {
            Ok(preview) => preview,
            Err(error @ ProviderError::EgressDenied { .. }) => {
                outcomes.push(refused(error.to_string(), 0));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let payload_bytes = serde_json::to_vec(&preview.payload)
            .map_err(|error| SemanticError::InvalidEval(error.to_string()))?
            .len();
        let grant = consent(&preview);
        let started = Instant::now();
        match firewall.decide(provider, tier, &action_id, &request, grant.as_ref()) {
            Ok(response) => {
                let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
                let answer = &response.answers[0];
                let chosen = case.candidates.get(answer.choice);
                outcomes.push(DecisionOutcome {
                    case_id: case.case_id.clone(),
                    chosen: chosen.cloned(),
                    confidence: Some(answer.confidence),
                    correct: scored(case, chosen),
                    refused: None,
                    latency_ms: Some(latency_ms),
                    payload_bytes,
                    reported_input_tokens: response.reported_input_tokens,
                });
            }
            Err(
                error @ (ProviderError::ConsentRequired(_)
                | ProviderError::ConsentMismatch
                | ProviderError::EgressUnrecorded { .. }),
            ) => outcomes.push(refused(error.to_string(), payload_bytes)),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(outcomes)
}

/// The local embedding resolver's top-1 on `cases`: the candidate most
/// similar to the state, or `none` below `threshold`. Precision over all
/// cases, or `None` for no cases.
pub fn local_precision_at_1(
    provider: &dyn LlmProvider,
    cases: &[DecisionCase],
    threshold: f32,
) -> Result<Option<f64>, SemanticError> {
    if cases.is_empty() {
        return Ok(None);
    }
    let mut correct = 0usize;
    for case in cases {
        let mut texts = vec![case.state.clone()];
        texts.extend(case.candidates.iter().cloned());
        let embeddings = provider.embed(&texts)?;
        let mut best: Option<(f32, &String)> = None;
        for (candidate, embedding) in case.candidates.iter().zip(&embeddings[1..]) {
            let similarity = crate::cosine(&embeddings[0], embedding)?;
            if best.is_none_or(|(top, _)| similarity > top) {
                best = Some((similarity, candidate));
            }
        }
        let chosen = best
            .filter(|(similarity, _)| *similarity >= threshold)
            .map(|(_, id)| id);
        correct += usize::from(scored(case, chosen));
    }
    Ok(Some(correct as f64 / cases.len() as f64))
}

fn percentile(sorted: &[f64], share: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    // Nearest rank.
    let rank = (share * sorted.len() as f64).ceil().max(1.0) as usize;
    Some(sorted[rank.min(sorted.len()) - 1])
}

/// The report over `outcomes`.
pub fn report(
    provider_id: &str,
    outcomes: Vec<DecisionOutcome>,
    price_per_million_input_tokens: f64,
    local_precision_at_1: Option<f64>,
) -> DecisionReport {
    let answered: Vec<&DecisionOutcome> = outcomes
        .iter()
        .filter(|outcome| outcome.confidence.is_some())
        .collect();
    let n = answered.len();
    let precision_at_1 =
        (n > 0).then(|| answered.iter().filter(|o| o.correct).count() as f64 / n as f64);
    let width = 1.0 / CALIBRATION_BINS as f64;
    let mut calibration = Vec::new();
    let mut ece = 0.0;
    for bin in 0..CALIBRATION_BINS {
        let lower = bin as f64 * width;
        let upper = lower + width;
        let members: Vec<&&DecisionOutcome> = answered
            .iter()
            .filter(|o| {
                let c = o.confidence.unwrap_or_default();
                // Scale before flooring so 0.7 lands in [0.7, 0.8), not [0.6, 0.7).
                let index =
                    ((c * CALIBRATION_BINS as f64 + 1e-9) as usize).min(CALIBRATION_BINS - 1);
                index == bin
            })
            .collect();
        if members.is_empty() {
            continue;
        }
        let count = members.len();
        let mean_confidence = members
            .iter()
            .map(|o| o.confidence.unwrap_or_default())
            .sum::<f64>()
            / count as f64;
        let accuracy = members.iter().filter(|o| o.correct).count() as f64 / count as f64;
        ece += (count as f64 / n as f64) * (accuracy - mean_confidence).abs();
        calibration.push(CalibrationBin {
            lower,
            upper,
            count,
            mean_confidence,
            accuracy,
        });
    }
    let mut latencies: Vec<f64> = answered.iter().filter_map(|o| o.latency_ms).collect();
    latencies.sort_by(f64::total_cmp);
    let input_tokens = (n > 0)
        .then(|| {
            answered
                .iter()
                .map(|o| o.reported_input_tokens)
                .sum::<Option<u64>>()
        })
        .flatten();
    DecisionReport {
        provider_id: provider_id.to_owned(),
        answered: n,
        refused: outcomes.iter().filter(|o| o.refused.is_some()).count(),
        precision_at_1,
        calibration,
        expected_calibration_error: (n > 0).then_some(ece),
        latency_p50_ms: percentile(&latencies, 0.5),
        latency_p95_ms: percentile(&latencies, 0.95),
        input_tokens,
        input_cost_usd: input_tokens
            .map(|tokens| tokens as f64 / 1_000_000.0 * price_per_million_input_tokens),
        local_precision_at_1,
        outcomes,
    }
}

#[cfg(test)]
#[path = "decision_eval_tests.rs"]
mod tests;
