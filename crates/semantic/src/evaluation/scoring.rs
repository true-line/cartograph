//! Score sealed visible-fixture predictions. No receipt here authorizes model quality.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::{
    fixtures::{CaseLabel, LoadedFixtureBundle, Split, Truth},
    policy::{self, DatasetBinding, PredictionChoice, SealedPredictions},
    report::{
        DecisionReason, EvaluationSummary, FamilyDecision, FamilyStatus, QualityMetrics,
        SummaryMode,
    },
};
use crate::FamilyKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Evaluation labels enter only after prediction has returned an owned seal.
#[derive(Debug)]
pub struct EvaluationLabels {
    binding: DatasetBinding,
    labels: Vec<CaseLabel>,
}
impl EvaluationLabels {
    pub fn from_bundle(bundle: &LoadedFixtureBundle) -> Self {
        let ids = policy::split_cases(bundle, Split::Evaluation);
        Self {
            binding: DatasetBinding::from_bundle(bundle),
            labels: bundle
                .labels()
                .labels
                .iter()
                .filter(|label| ids.contains(label.case_id.as_str()))
                .cloned()
                .collect(),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoringCounts {
    pub cases: u64,
    pub answerable: u64,
    pub no_match: u64,
    pub unknown: u64,
    pub true_positives: u64,
    pub false_positives: u64,
    pub false_negatives: u64,
    pub no_match_false_admissions: u64,
    pub admitted: u64,
    pub abstained: u64,
    pub retrieved_answerable: u64,
    pub staged_proposals: u64,
    pub ambiguous_proposals: u64,
}
#[derive(Debug)]
pub struct FamilyScore {
    family: FamilyKey,
    counts: ScoringCounts,
    adequate: bool,
    passed: bool,
}
impl FamilyScore {
    pub fn family(&self) -> &FamilyKey {
        &self.family
    }
    pub fn counts(&self) -> &ScoringCounts {
        &self.counts
    }
    pub fn adequate(&self) -> bool {
        self.adequate
    }
    pub fn passed(&self) -> bool {
        self.passed
    }
    pub fn metrics(&self) -> QualityMetrics {
        let c = &self.counts;
        let ratio = |n: u64, d: u64| if d == 0 { 0.0 } else { n as f64 / d as f64 };
        QualityMetrics {
            precision: ratio(c.true_positives, c.true_positives + c.false_positives),
            recall: ratio(c.true_positives, c.answerable),
            true_positives: c.true_positives,
            false_positives: c.false_positives,
            false_negatives: c.false_negatives,
            unknown: c.unknown,
            no_match_false_admissions: c.no_match_false_admissions,
            retrieval_recall: Some(ratio(c.retrieved_answerable, c.answerable)),
        }
    }
}
/// Constructed only by scoring, never Deserialize/Default or a public unchecked
/// constructor. Its purpose is fixed: visible mechanism, never quality authority.
#[derive(Debug)]
pub struct MechanismReceipt {
    sealed: SealedPredictions,
    scores: Vec<FamilyScore>,
    digest: String,
}
impl MechanismReceipt {
    pub fn purpose(&self) -> &'static str {
        "visible_mechanism_fixture"
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn policy_digest(&self) -> &str {
        self.sealed.policy_digest()
    }
    pub fn prediction_digest(&self) -> &str {
        self.sealed.digest()
    }
    pub fn scores(&self) -> &[FamilyScore] {
        &self.scores
    }
    pub fn summary(&self) -> Result<EvaluationSummary, String> {
        let summary = EvaluationSummary {
            schema_version: 1,
            mode: SummaryMode::VisibleMechanismEvaluation,
            families: self
                .scores
                .iter()
                .map(|score| {
                    let reason = if !score.adequate {
                        DecisionReason::InsufficientEvidence
                    } else if score.passed {
                        DecisionReason::IndependentlyEvaluated
                    } else {
                        DecisionReason::BelowFloor
                    };
                    FamilyDecision {
                        family: score.family.clone(),
                        status: if score.adequate {
                            FamilyStatus::IndependentlyEvaluated
                        } else {
                            FamilyStatus::Abstained
                        },
                        reason,
                        staged: score.counts.staged_proposals,
                        admitted: if score.passed {
                            score.counts.admitted
                        } else {
                            0
                        },
                        ambiguous: score.counts.ambiguous_proposals,
                        receipt_digest: Some(self.digest.clone()),
                        metrics: Some(score.metrics()),
                    }
                })
                .collect(),
        };
        summary.validate()?;
        Ok(summary)
    }
}

pub(super) fn score_predictions(
    sealed: &SealedPredictions,
    labels: &EvaluationLabels,
) -> Result<Vec<FamilyScore>, String> {
    if sealed.policy.binding != labels.binding {
        return Err("scoring dataset mismatch".into());
    }
    let by_id: BTreeMap<_, _> = labels
        .labels
        .iter()
        .map(|label| (label.case_id.as_str(), label))
        .collect();
    let ids: BTreeSet<_> = sealed
        .predictions
        .iter()
        .map(|prediction| prediction.case.case_id.as_str())
        .collect();
    if by_id.len() != labels.labels.len()
        || ids.len() != sealed.predictions.len()
        || ids != by_id.keys().copied().collect()
    {
        return Err("scoring label coverage mismatch".into());
    }
    let mut counts: BTreeMap<FamilyKey, ScoringCounts> = sealed
        .policy
        .families
        .keys()
        .cloned()
        .map(|family| (family, ScoringCounts::default()))
        .collect();
    for prediction in &sealed.predictions {
        let Some(c) = counts.get_mut(&prediction.case.family) else {
            if prediction.choice != PredictionChoice::MissingPolicy || !prediction.ranked.is_empty()
            {
                return Err("unconfigured scoring family has a prediction".into());
            }
            continue;
        };
        if prediction.choice == PredictionChoice::MissingPolicy {
            return Err("configured scoring family is missing its prediction".into());
        }
        c.cases += 1;
        c.staged_proposals += prediction.ranked.len() as u64;
        let admitted = match &prediction.choice {
            PredictionChoice::Admitted { target_id } => {
                c.admitted += 1;
                Some(target_id.as_str())
            }
            _ => {
                c.abstained += 1;
                None
            }
        };
        if prediction.choice == PredictionChoice::Ambiguous {
            c.ambiguous_proposals += prediction.ranked.len() as u64;
        }
        match &by_id[prediction.case.case_id.as_str()].truth {
            Truth::AcceptableTargets { target_ids } => {
                c.answerable += 1;
                if prediction
                    .ranked
                    .iter()
                    .any(|p| target_ids.contains(&p.target_id))
                {
                    c.retrieved_answerable += 1;
                }
                if admitted.is_some_and(|target| target_ids.iter().any(|id| id == target)) {
                    c.true_positives += 1;
                } else {
                    c.false_negatives += 1;
                    if admitted.is_some() {
                        c.false_positives += 1;
                    }
                }
            }
            Truth::NoMatch => {
                c.no_match += 1;
                if admitted.is_some() {
                    c.false_positives += 1;
                    c.no_match_false_admissions += 1;
                }
            }
            Truth::Unknown { .. } => {
                c.unknown += 1;
            }
        }
    }
    let scores: Vec<_> = counts
        .into_iter()
        .map(|(family, counts)| {
            let p = &sealed.policy.families[&family];
            let known = counts.answerable + counts.no_match;
            let adequate = known >= p.min_evaluation_cases
                && counts.answerable >= p.min_answerable_cases
                && counts.no_match >= p.min_no_match_cases
                && sealed
                    .policy
                    .calibration_counts
                    .get(&family)
                    .copied()
                    .unwrap_or(0)
                    > 0;
            let precision = if counts.true_positives + counts.false_positives == 0 {
                0.0
            } else {
                counts.true_positives as f64
                    / (counts.true_positives + counts.false_positives) as f64
            };
            let passed = adequate && counts.true_positives > 0 && precision >= p.precision_floor;
            FamilyScore {
                family,
                counts,
                adequate,
                passed,
            }
        })
        .collect();
    Ok(scores)
}

pub(super) fn scores_digest(
    sealed: &SealedPredictions,
    scores: &[FamilyScore],
    purpose: &str,
) -> Result<String, String> {
    let records:Vec<_>=scores.iter().map(|score|serde_json::json!({"family":score.family,"counts":score.counts,"adequate":score.adequate,"passed":score.passed})).collect();
    let digest = policy::digest(
        &serde_json::json!({"recipe":"t2-fixed-policy-scoring-receipt@1","purpose":purpose,"policy":sealed.policy_digest(),"predictions":sealed.digest(),"binding":sealed.policy.binding,"scores":records}),
    )?;
    Ok(digest)
}

pub fn score(
    sealed: SealedPredictions,
    labels: EvaluationLabels,
) -> Result<MechanismReceipt, String> {
    if sealed.policy.binding.purpose != "visible_mechanism_fixture" {
        return Err("mechanism scoring requires visible fixture purpose".into());
    }
    let scores = score_predictions(&sealed, &labels)?;
    let digest = scores_digest(&sealed, &scores, "visible_mechanism_fixture")?;
    Ok(MechanismReceipt {
        sealed,
        scores,
        digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        HopKind,
        embedding_cache::{CachedProvider, EmbeddingCache},
        evaluation::policy::{
            CalibrationView, EvaluationInputs, FamilyPolicy, FrozenPolicy, planned_embedding_texts,
            predict,
        },
    };
    use llm::{Embedding, EmbeddingIdentity, LlmProvider, Locality, ProviderCaps, ProviderError};
    use std::{
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    struct Scripted {
        calls: AtomicUsize,
        false_match: bool,
    }
    impl LlmProvider for Scripted {
        fn id(&self) -> &str {
            "scripted-visible-mechanism"
        }
        fn locality(&self) -> Locality {
            Locality::Local
        }
        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: true,
                chat: false,
                tool_use: false,
            }
        }
        fn embedding_cache_identity(&self) -> Result<Option<EmbeddingIdentity>, ProviderError> {
            Ok(Some(EmbeddingIdentity {
                endpoint: "scripted://visible".into(),
                model: "mechanism".into(),
                digest: format!("sha256:{}", "a".repeat(64)),
                recipe: "scripted-v1".into(),
            }))
        }
        fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(batch
                .iter()
                .map(|text| {
                    if text.contains("chargeCard")
                        || (self.false_match && text.contains("reconcileLedger"))
                    {
                        vec![1.0, 0.0, 0.0]
                    } else if text.contains("refundCard") {
                        vec![0.0, 1.0, 0.0]
                    } else if text.contains("formatCurrency") {
                        vec![0.0, 0.0, 1.0]
                    } else {
                        vec![1.0, 1.0, 1.0]
                    }
                })
                .collect())
        }
    }
    fn fixture() -> LoadedFixtureBundle {
        super::super::fixtures::load_fixture_bundle(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/t2-visible-mechanism-v1"),
        )
        .unwrap()
    }
    fn parameters() -> Vec<FamilyPolicy> {
        // Fixed BEFORE results: deliberately greater than this thin visible
        // fixture's per-family coverage. Never lowered to produce a pass.
        [
            FamilyKey::new(HopKind::Call, None),
            FamilyKey::new(HopKind::Channel, None),
            FamilyKey::new(HopKind::Channel, Some("sqs-queue".into())),
            FamilyKey::new(HopKind::Channel, Some("sns-topic".into())),
        ]
        .into_iter()
        .map(|family| FamilyPolicy {
            family,
            threshold: 0.9,
            ambiguity_margin: 0.01,
            neighbors: 3,
            min_evaluation_cases: 10,
            min_answerable_cases: 3,
            min_no_match_cases: 3,
            precision_floor: 0.95,
        })
        .collect()
    }
    fn run(false_match: bool) -> (MechanismReceipt, usize) {
        let bundle = fixture();
        let calibration = CalibrationView::from_bundle(&bundle);
        let inputs = EvaluationInputs::from_bundle(&bundle);
        let provider = Scripted {
            calls: AtomicUsize::new(0),
            false_match,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut cache = EmbeddingCache::open(dir.path().join("cache.db")).unwrap();
        let lease = cache.bind_instance(&"a".repeat(32)).unwrap();
        let cached = CachedProvider::new(&provider, Arc::new(Mutex::new(cache)), lease).unwrap();
        let texts = planned_embedding_texts(&calibration, &inputs).unwrap();
        cached.embed(&texts).unwrap();
        cached.seal().unwrap();
        let binding = cached.finish_with_binding().unwrap().1.unwrap();
        let policy = FrozenPolicy::freeze(&calibration, &binding, parameters()).unwrap();
        let before = provider.calls.load(Ordering::SeqCst);
        let predictions = predict(&policy, &inputs, &cached).unwrap();
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            before,
            "prediction uses sealed actual namespace vectors"
        );
        let receipt = score(predictions, EvaluationLabels::from_bundle(&bundle)).unwrap();
        (receipt, provider.calls.load(Ordering::SeqCst))
    }
    // AC-0272/0273/0275: real production propose ranks the frozen universe;
    // multiple acceptable tied targets abstain rather than using lexical order.
    #[test]
    fn mechanism_scoring_runs_production_predictions_and_preserves_thin_family_abstention() {
        let (receipt, calls) = run(false);
        assert!(calls > 0);
        assert_eq!(receipt.purpose(), "visible_mechanism_fixture");
        let call = receipt
            .scores()
            .iter()
            .find(|s| s.family() == &FamilyKey::new(HopKind::Call, None))
            .unwrap();
        let counts = call.counts();
        assert_eq!(
            (counts.cases, counts.answerable, counts.no_match),
            (5, 3, 2)
        );
        assert_eq!(
            (
                counts.true_positives,
                counts.false_positives,
                counts.false_negatives
            ),
            (2, 0, 1)
        );
        assert_eq!(
            (
                counts.admitted,
                counts.abstained,
                counts.retrieved_answerable
            ),
            (2, 3, 3)
        );
        assert!(counts.ambiguous_proposals >= 2);
        assert!(!call.adequate());
        assert!(!call.passed());
        let summary = receipt.summary().unwrap();
        assert_eq!(summary.mode, SummaryMode::VisibleMechanismEvaluation);
        assert!(
            summary
                .families
                .iter()
                .all(|f| f.reason == DecisionReason::InsufficientEvidence && f.admitted == 0)
        );
        assert!(
            receipt
                .scores()
                .iter()
                .filter(|s| s.family().kind() == HopKind::Channel)
                .all(|s| s.counts().cases == 0 && !s.passed())
        );
        let (same, _) = run(false);
        assert_eq!(receipt.digest(), same.digest());
    }
    // AC-0273/0275: an actual no-match false admission is counted independently
    // of answerable recall and cannot be concealed by the insufficient verdict.
    #[test]
    fn mechanism_scoring_counts_no_match_false_admission_without_quality_claim() {
        let (receipt, _) = run(true);
        let call = receipt
            .scores()
            .iter()
            .find(|s| s.family().kind() == HopKind::Call)
            .unwrap();
        assert_eq!(call.counts().no_match_false_admissions, 1);
        assert_eq!(call.counts().false_positives, 1);
        assert_eq!(call.metrics().recall, 2.0 / 3.0);
        assert_eq!(call.metrics().precision, 2.0 / 3.0);
        assert!(!call.passed());
        assert_ne!(receipt.digest(), run(false).0.digest());
    }
    // AC-0272/0274: explicit minima, finite policies and exact sealed namespace
    // are checked before any ANN prediction; labels cannot replace a seal.
    #[test]
    fn mechanism_policy_rejects_invalid_configuration_and_unsealed_predictions() {
        let bundle = fixture();
        let calibration = CalibrationView::from_bundle(&bundle);
        let inputs = EvaluationInputs::from_bundle(&bundle);
        assert!(
            calibration
                .labels()
                .iter()
                .all(|label| !inputs.cases().iter().any(|c| c.case_id == label.case_id))
        );
        let provider = Scripted {
            calls: AtomicUsize::new(0),
            false_match: false,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut cache = EmbeddingCache::open(dir.path().join("cache.db")).unwrap();
        let lease = cache.bind_instance(&"a".repeat(32)).unwrap();
        let cached = CachedProvider::new(&provider, Arc::new(Mutex::new(cache)), lease).unwrap();
        cached
            .embed(&planned_embedding_texts(&calibration, &inputs).unwrap())
            .unwrap();
        cached.seal().unwrap();
        let binding = cached.finish_with_binding().unwrap().1.unwrap();
        for mode in 0..4 {
            let mut params = parameters();
            match mode {
                0 => params[0].threshold = f32::NAN,
                1 => params[0].precision_floor = f64::INFINITY,
                2 => params[0].min_evaluation_cases = 0,
                _ => params[0].neighbors = 1,
            };
            assert!(FrozenPolicy::freeze(&calibration, &binding, params).is_err());
        }
        let frozen = FrozenPolicy::freeze(&calibration, &binding, parameters()).unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        let mut other = EmbeddingCache::open(other_dir.path().join("cache.db")).unwrap();
        let lease = other.bind_instance(&"a".repeat(32)).unwrap();
        let unsealed = CachedProvider::new(&provider, Arc::new(Mutex::new(other)), lease).unwrap();
        let before = provider.calls.load(Ordering::SeqCst);
        assert!(predict(&frozen, &inputs, &unsealed).is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), before);
        let sealed = predict(&frozen, &inputs, &cached).unwrap();
        let mut labels = EvaluationLabels::from_bundle(&bundle);
        labels.labels.pop();
        assert!(score(sealed, labels).is_err());
    }
}
