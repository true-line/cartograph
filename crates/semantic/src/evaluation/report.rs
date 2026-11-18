//! Descriptive evaluation metadata for persistence and display, never admission authority.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use crate::FamilyKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_SUMMARY_BYTES: usize = 64 * 1024;
pub const MAX_SUMMARY_FAMILIES: usize = 256;
pub const MAX_SUMMARY_COUNT: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryMode {
    CalibrationOnly,
    IndependentEvaluation,
    /// Scripted visible fixtures establish mechanism behavior, never model quality.
    VisibleMechanismEvaluation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FamilyStatus {
    CalibrationOnly,
    IndependentlyEvaluated,
    Abstained,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    CalibrationOnly,
    MissingEvidence,
    IncompatibleEvidence,
    InsufficientEvidence,
    BelowFloor,
    IndependentlyEvaluated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityMetrics {
    pub precision: f64,
    pub recall: f64,
    pub true_positives: u64,
    pub false_positives: u64,
    pub false_negatives: u64,
    pub unknown: u64,
    pub no_match_false_admissions: u64,
    pub retrieval_recall: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyDecision {
    pub family: FamilyKey,
    pub status: FamilyStatus,
    pub reason: DecisionReason,
    pub staged: u64,
    pub admitted: u64,
    /// Individual staged proposals withheld for ambiguity, not a family verdict.
    pub ambiguous: u64,
    pub receipt_digest: Option<String>,
    pub metrics: Option<QualityMetrics>,
}

/// Decoding/validating this display record does not construct a trusted receipt
/// or independently verify the dataset, policy, predictions or their provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationSummary {
    pub schema_version: u32,
    pub mode: SummaryMode,
    pub families: Vec<FamilyDecision>,
}

fn unit(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}
fn count_sum(values: &[u64]) -> Result<u64, String> {
    values.iter().try_fold(0_u64, |sum, value| {
        if *value > MAX_SUMMARY_COUNT {
            return Err("evaluation count exceeds bound".into());
        }
        sum.checked_add(*value)
            .ok_or_else(|| "evaluation count overflow".into())
    })
}
impl QualityMetrics {
    fn validate(&self) -> Result<(), String> {
        if !unit(self.precision)
            || !unit(self.recall)
            || self.retrieval_recall.is_some_and(|value| !unit(value))
        {
            return Err("evaluation metrics must be finite unit values".into());
        }
        count_sum(&[
            self.true_positives,
            self.false_positives,
            self.false_negatives,
            self.unknown,
        ])?;
        count_sum(&[self.no_match_false_admissions])?;
        if self.no_match_false_admissions > self.false_positives {
            return Err("no-match false admissions exceed false positives".into());
        }
        let predicted = count_sum(&[self.true_positives, self.false_positives])?;
        let answerable = count_sum(&[self.true_positives, self.false_negatives])?;
        let ratio = |denominator: u64| {
            if denominator == 0 {
                0.0
            } else {
                self.true_positives as f64 / denominator as f64
            }
        };
        if (self.precision - ratio(predicted)).abs() > 1.0e-6
            || (self.recall - ratio(answerable)).abs() > 1.0e-6
        {
            return Err("evaluation metrics disagree with counts".into());
        }
        Ok(())
    }
}
impl EvaluationSummary {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_SUMMARY_BYTES {
            return Err("evaluation summary exceeds byte bound".into());
        }
        let summary: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        summary.validate()?;
        Ok(summary)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 || self.families.len() > MAX_SUMMARY_FAMILIES {
            return Err("unsupported evaluation summary version or family count".into());
        }
        let mut seen = BTreeSet::new();
        for family in &self.families {
            if family
                .family
                .subtype()
                .is_some_and(|value| value.len() > 128)
                || !seen.insert(&family.family)
            {
                return Err("invalid or duplicate evaluation family".into());
            }
            count_sum(&[family.staged])?;
            if count_sum(&[family.admitted, family.ambiguous])? > family.staged {
                return Err("evaluation admission counts exceed staged proposals".into());
            }
            if let Some(digest) = &family.receipt_digest
                && (digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            {
                return Err("invalid evaluation receipt digest".into());
            }
            if let Some(metrics) = &family.metrics {
                metrics.validate()?;
            }
            match (self.mode, family.status, family.reason) {
                (
                    SummaryMode::CalibrationOnly,
                    FamilyStatus::CalibrationOnly,
                    DecisionReason::CalibrationOnly,
                ) if family.admitted == 0 && family.receipt_digest.is_none() => {}
                (
                    SummaryMode::IndependentEvaluation | SummaryMode::VisibleMechanismEvaluation,
                    FamilyStatus::IndependentlyEvaluated,
                    reason,
                ) if matches!(
                    reason,
                    DecisionReason::IndependentlyEvaluated | DecisionReason::BelowFloor
                ) && family.receipt_digest.is_some()
                    && family.metrics.is_some()
                    && (reason != DecisionReason::BelowFloor || family.admitted == 0) => {}
                (
                    SummaryMode::IndependentEvaluation | SummaryMode::VisibleMechanismEvaluation,
                    FamilyStatus::Abstained,
                    reason,
                ) if matches!(
                    reason,
                    DecisionReason::MissingEvidence
                        | DecisionReason::IncompatibleEvidence
                        | DecisionReason::InsufficientEvidence
                ) && family.admitted == 0 => {}
                _ => return Err("inconsistent evaluation mode, status, reason or admission".into()),
            }
        }
        if serde_json::to_vec(self)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_SUMMARY_BYTES
        {
            return Err("evaluation summary exceeds byte bound".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HopKind;

    fn summary() -> EvaluationSummary {
        EvaluationSummary {
            schema_version: 1,
            mode: SummaryMode::IndependentEvaluation,
            families: vec![FamilyDecision {
                family: FamilyKey::new(HopKind::Call, None),
                status: FamilyStatus::IndependentlyEvaluated,
                reason: DecisionReason::IndependentlyEvaluated,
                staged: 3,
                admitted: 1,
                ambiguous: 1,
                receipt_digest: Some("a".repeat(64)),
                metrics: Some(QualityMetrics {
                    precision: 1.0,
                    recall: 0.5,
                    true_positives: 2,
                    false_positives: 0,
                    false_negatives: 2,
                    unknown: 1,
                    no_match_false_admissions: 0,
                    retrieval_recall: Some(0.75),
                }),
            }],
        }
    }

    // AC-0271/0278: descriptive family outcomes remain distinct and calibrated
    // or unevaluated families never claim admitted proposals.
    #[test]
    fn evaluation_summary_distinguishes_calibration_independent_and_abstention() {
        let mut value = summary();
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(EvaluationSummary::decode(&bytes).unwrap(), value);
        value.mode = SummaryMode::VisibleMechanismEvaluation;
        value.validate().unwrap();
        let encoded = serde_json::to_value(&value).unwrap();
        assert_eq!(encoded["mode"], "visible_mechanism_evaluation");
        value.mode = SummaryMode::IndependentEvaluation;
        value.families[0].reason = DecisionReason::BelowFloor;
        assert!(value.validate().is_err());
        value.families[0].admitted = 0;
        value.validate().unwrap();
        for reason in [
            DecisionReason::MissingEvidence,
            DecisionReason::IncompatibleEvidence,
            DecisionReason::InsufficientEvidence,
        ] {
            value.families[0].status = FamilyStatus::Abstained;
            value.families[0].reason = reason;
            value.families[0].metrics = None;
            value.families[0].receipt_digest = None;
            value.validate().unwrap();
        }
        value.mode = SummaryMode::CalibrationOnly;
        value.families[0].status = FamilyStatus::CalibrationOnly;
        value.families[0].reason = DecisionReason::CalibrationOnly;
        value.validate().unwrap();
        value.families[0].admitted = 1;
        assert!(value.validate().is_err());
        for mode in [
            SummaryMode::CalibrationOnly,
            SummaryMode::IndependentEvaluation,
            SummaryMode::VisibleMechanismEvaluation,
        ] {
            EvaluationSummary {
                schema_version: 1,
                mode,
                families: vec![],
            }
            .validate()
            .unwrap();
        }
    }

    // AC-0278: corrupt persistence metadata cannot silently change displayed
    // counts, metrics, family identity or mode; decoding adds no admission API.
    #[test]
    fn evaluation_summary_rejects_corrupt_metrics_counts_shape_and_bounds() {
        let original = summary();
        let mut value = original.clone();
        value.families.push(value.families[0].clone());
        assert!(value.validate().is_err());
        for count in [4, u64::MAX] {
            let mut value = original.clone();
            value.families[0].admitted = count;
            assert!(value.validate().is_err());
        }
        for metric in [f64::NAN, f64::INFINITY, -0.1, 1.1, 0.7] {
            let mut value = original.clone();
            value.families[0].metrics.as_mut().unwrap().precision = metric;
            assert!(value.validate().is_err());
        }
        let mut value = serde_json::to_value(&original).unwrap();
        value["families"][0]["passed"] = serde_json::json!(true);
        assert!(EvaluationSummary::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(EvaluationSummary::decode(br#"{"schema_version":1,"schema_version":1,"mode":"calibration_only","families":[]}"#).is_err());
        assert!(EvaluationSummary::decode(&vec![b' '; MAX_SUMMARY_BYTES + 1]).is_err());
        let mut value = original;
        value.families[0].receipt_digest = Some("A".repeat(64));
        assert!(value.validate().is_err());
    }
}
