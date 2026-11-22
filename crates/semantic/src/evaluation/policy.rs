//! Frozen explicit mechanism policies and label-free prediction through production ANN.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::fixtures::{self, CaseInput, CaseLabel, LoadedFixtureBundle, Split};
use crate::{
    FamilyKey, SemanticProposal,
    embedding_cache::{CachedProvider, EmbeddingSpaceBinding},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Result<T> = std::result::Result<T, String>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct DatasetBinding {
    pub dataset: String,
    pub purpose: String,
    pub inputs: String,
    pub leakage: String,
    pub labels: String,
}
impl DatasetBinding {
    pub(super) fn from_bundle(bundle: &LoadedFixtureBundle) -> Self {
        Self {
            dataset: bundle.inputs().dataset_id.clone(),
            purpose: bundle.purpose().to_owned(),
            inputs: bundle.digests().inputs.clone(),
            leakage: bundle.digests().leakage.clone(),
            labels: bundle.digests().labels.clone(),
        }
    }
}
pub(super) fn split_cases(bundle: &LoadedFixtureBundle, split: Split) -> BTreeSet<&str> {
    bundle
        .leakage()
        .groups
        .iter()
        .filter(|group| group.split == split)
        .flat_map(|group| group.case_ids.iter().map(String::as_str))
        .collect()
}
pub(super) fn digest(value: &impl Serialize) -> Result<String> {
    fixtures::canonical_json(value)
        .map(|bytes| fixtures::sha256(&bytes))
        .map_err(|error| error.to_string())
}

/// This view exposes calibration labels only, not evaluation labels.
#[derive(Debug)]
pub struct CalibrationView {
    pub(super) binding: DatasetBinding,
    cases: Vec<CaseInput>,
    labels: Vec<CaseLabel>,
}
impl CalibrationView {
    pub fn from_bundle(bundle: &LoadedFixtureBundle) -> Self {
        let ids = split_cases(bundle, Split::Calibration);
        Self {
            binding: DatasetBinding::from_bundle(bundle),
            cases: bundle
                .inputs()
                .cases
                .iter()
                .filter(|case| ids.contains(case.case_id.as_str()))
                .cloned()
                .collect(),
            labels: bundle
                .labels()
                .labels
                .iter()
                .filter(|label| ids.contains(label.case_id.as_str()))
                .cloned()
                .collect(),
        }
    }
    pub fn cases(&self) -> &[CaseInput] {
        &self.cases
    }
    pub fn labels(&self) -> &[CaseLabel] {
        &self.labels
    }
}
/// Validated inputs and partition binding, with no labels in this capability.
#[derive(Debug)]
pub struct EvaluationInputs {
    pub(super) binding: DatasetBinding,
    pub(super) cases: Vec<CaseInput>,
}
impl EvaluationInputs {
    pub fn from_bundle(bundle: &LoadedFixtureBundle) -> Self {
        let ids = split_cases(bundle, Split::Evaluation);
        Self {
            binding: DatasetBinding::from_bundle(bundle),
            cases: bundle
                .inputs()
                .cases
                .iter()
                .filter(|case| ids.contains(case.case_id.as_str()))
                .cloned()
                .collect(),
        }
    }
    pub fn cases(&self) -> &[CaseInput] {
        &self.cases
    }
}

#[derive(Debug, Clone)]
pub struct FamilyPolicy {
    pub family: FamilyKey,
    pub threshold: f32,
    pub ambiguity_margin: f32,
    pub neighbors: usize,
    pub min_evaluation_cases: u64,
    pub min_answerable_cases: u64,
    pub min_no_match_cases: u64,
    pub precision_floor: f64,
}
/// A supplied operating point frozen before holdout prediction. This constructor
/// does not claim that any calibration procedure selected the supplied values.
#[derive(Debug, Clone)]
pub struct FrozenPolicy {
    pub(super) binding: DatasetBinding,
    pub(super) embedding: EmbeddingSpaceBinding,
    pub(super) families: BTreeMap<FamilyKey, FamilyPolicy>,
    pub(super) calibration_counts: BTreeMap<FamilyKey, u64>,
    pub(super) digest: String,
}
impl FrozenPolicy {
    pub fn freeze(
        calibration: &CalibrationView,
        embedding: &EmbeddingSpaceBinding,
        parameters: Vec<FamilyPolicy>,
    ) -> Result<Self> {
        let (families, digest) = freeze_parameters(&calibration.binding, embedding, parameters)?;
        let known_calibration: BTreeSet<_> = calibration
            .labels
            .iter()
            .filter(|label| !matches!(label.truth, fixtures::Truth::Unknown { .. }))
            .map(|label| label.case_id.as_str())
            .collect();
        let mut calibration_counts = BTreeMap::new();
        for case in &calibration.cases {
            if !known_calibration.contains(case.case_id.as_str()) {
                continue;
            }
            *calibration_counts
                .entry(case.family.clone())
                .or_insert(0_u64) += 1;
        }
        Ok(Self {
            binding: calibration.binding.clone(),
            embedding: embedding.clone(),
            families,
            calibration_counts,
            digest,
        })
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn embedding_binding(&self) -> &EmbeddingSpaceBinding {
        &self.embedding
    }
    pub fn family_policies(&self) -> impl Iterator<Item = &FamilyPolicy> {
        self.families.values()
    }
    /// The policy a locked receipt was scored under, rebuilt from the
    /// receipt's dataset digests and the host's own parameters. It carries no
    /// calibration counts, so it can rank live hops but never score.
    pub(super) fn for_locked_receipt(
        binding: DatasetBinding,
        embedding: &EmbeddingSpaceBinding,
        parameters: Vec<FamilyPolicy>,
    ) -> Result<Self> {
        let (families, digest) = freeze_parameters(&binding, embedding, parameters)?;
        Ok(Self {
            binding,
            embedding: embedding.clone(),
            families,
            calibration_counts: BTreeMap::new(),
            digest,
        })
    }
}

/// Validate explicit family parameters and compute the policy digest
/// (`t2-explicit-policy-bits@1`). The digest binds the dataset and embedding
/// space, never calibration labels, so a host holding only a locked receipt's
/// dataset digests recomputes it exactly (ADR-0082).
fn freeze_parameters(
    binding: &DatasetBinding,
    embedding: &EmbeddingSpaceBinding,
    parameters: Vec<FamilyPolicy>,
) -> Result<(BTreeMap<FamilyKey, FamilyPolicy>, String)> {
    if parameters.is_empty() || parameters.len() > 256 {
        return Err("policy family bound".into());
    }
    let mut families = BTreeMap::new();
    for parameter in parameters {
        if !parameter.threshold.is_finite()
            || !(0.0..=1.0).contains(&parameter.threshold)
            || !parameter.ambiguity_margin.is_finite()
            || !(0.0..=1.0).contains(&parameter.ambiguity_margin)
            || !parameter.precision_floor.is_finite()
            || !(0.0..=1.0).contains(&parameter.precision_floor)
            || !(2..=256).contains(&parameter.neighbors)
            || [
                parameter.min_evaluation_cases,
                parameter.min_answerable_cases,
                parameter.min_no_match_cases,
            ]
            .iter()
            .any(|count| !(1..=1_000_000).contains(count))
            || parameter.family.subtype().is_some_and(|s| s.len() > 128)
        {
            return Err("invalid explicit family policy".into());
        }
        if families
            .insert(parameter.family.clone(), parameter)
            .is_some()
        {
            return Err("duplicate policy family".into());
        }
    }
    // Integer-only policy-hash@1: exact f32 bit patterns are u32; exact f64
    // bit patterns are decimal strings to stay within JCS integer bounds.
    let policies: Vec<_> = families.values().map(|p| serde_json::json!({
        "family": p.family, "threshold_bits":p.threshold.to_bits(),
        "margin_bits":p.ambiguity_margin.to_bits(), "neighbors":p.neighbors,
        "minimum_cases":p.min_evaluation_cases,"minimum_answerable":p.min_answerable_cases,
        "minimum_no_match":p.min_no_match_cases,"precision_floor_bits":p.precision_floor.to_bits().to_string()
    })).collect();
    let digest = digest(
        &serde_json::json!({"recipe":"t2-explicit-policy-bits@1", "candidate_recipe":"t2-production-propose-cosine@1", "binding":binding,"embedding":embedding,"families":policies}),
    )?;
    Ok((families, digest))
}

/// Prepare all texts before sealing the #657 embedding operation. No labels or
/// inferred correctness affect this input plan.
pub fn planned_embedding_texts(
    calibration: &CalibrationView,
    evaluation: &EvaluationInputs,
) -> Result<Vec<String>> {
    if calibration.binding != evaluation.binding {
        return Err("planning dataset mismatch".into());
    }
    let texts: Vec<_> = calibration
        .cases
        .iter()
        .chain(&evaluation.cases)
        .flat_map(|case| {
            std::iter::once(case.hop.query.clone()).chain(
                case.candidates
                    .iter()
                    .map(|candidate| candidate.text.clone()),
            )
        })
        .collect();
    if texts.len() > crate::embedding_cache::MAX_ROWS
        || texts
            .iter()
            .any(|text| text.len() > crate::embedding_cache::MAX_TEXT_BYTES)
        || texts
            .iter()
            .try_fold(0_usize, |sum, text| sum.checked_add(text.len()))
            .is_none_or(|sum| sum > crate::embedding_cache::MAX_INPUT_BYTES)
    {
        return Err("prediction text plan capacity".into());
    }
    Ok(texts)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionChoice {
    MissingPolicy,
    Admitted { target_id: String },
    BelowThreshold,
    Ambiguous,
    NoCandidates,
}
#[derive(Debug, Clone)]
pub(super) struct CasePrediction {
    pub case: CaseInput,
    pub ranked: Vec<SemanticProposal>,
    pub choice: PredictionChoice,
}
/// Private construction and owned results prevent replacement of scores after
/// prediction. Labels are supplied only to the separate scoring entrypoint.
#[derive(Debug)]
pub struct SealedPredictions {
    pub(super) policy: FrozenPolicy,
    pub(super) predictions: Vec<CasePrediction>,
    pub(super) digest: String,
}
impl SealedPredictions {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn policy_digest(&self) -> &str {
        self.policy.digest()
    }
}
pub(super) fn choose(ranked: &[SemanticProposal], policy: &FamilyPolicy) -> PredictionChoice {
    let Some(best) = ranked.first() else {
        return PredictionChoice::NoCandidates;
    };
    if best.similarity < policy.threshold {
        return PredictionChoice::BelowThreshold;
    }
    if ranked
        .get(1)
        .is_some_and(|next| best.similarity - next.similarity <= policy.ambiguity_margin)
    {
        return PredictionChoice::Ambiguous;
    }
    PredictionChoice::Admitted {
        target_id: best.target_id.clone(),
    }
}
fn evidence(values: &[fixtures::EvidenceRef]) -> Vec<core_prov::EvidenceRef> {
    values
        .iter()
        .map(|e| core_prov::EvidenceRef {
            repo: e.repo.clone(),
            path: e.path.clone(),
            byte_start: e.byte_start,
            byte_end: e.byte_end,
            commit_sha: e.commit_sha.clone(),
        })
        .collect()
}
pub fn predict(
    policy: &FrozenPolicy,
    inputs: &EvaluationInputs,
    provider: &CachedProvider<'_>,
) -> Result<SealedPredictions> {
    if policy.binding != inputs.binding {
        return Err("prediction dataset mismatch".into());
    }
    let check_binding = || -> Result<()> {
        let (_, binding) = provider
            .finish_with_binding()
            .map_err(|error| error.to_string())?;
        if binding.as_ref() != Some(&policy.embedding) {
            return Err("prediction embedding space mismatch".into());
        }
        Ok(())
    };
    check_binding()?;
    let mut predictions = Vec::new();
    for case in &inputs.cases {
        // Preserve every frozen case in the sealed receipt, including explicit
        // abstentions for families with no configured decision rule.
        let Some(family) = policy.families.get(&case.family) else {
            predictions.push(CasePrediction {
                case: case.clone(),
                ranked: Vec::new(),
                choice: PredictionChoice::MissingPolicy,
            });
            continue;
        };
        let retrieve = family.neighbors;
        let hop = crate::UnresolvedHop {
            gap_id: case.hop.gap_id.clone(),
            source_id: case.hop.source_id.clone(),
            edge_label: match case.hop.edge_label {
                fixtures::EdgeLabel::CALLS => "CALLS",
                fixtures::EdgeLabel::PUBLISHES => "PUBLISHES",
                fixtures::EdgeLabel::SUBSCRIBES => "SUBSCRIBES",
            }
            .into(),
            kind: case.family.kind(),
            subtype: case.family.subtype().map(str::to_owned),
            query: case.hop.query.clone(),
            evidence: evidence(&case.hop.evidence),
            // Defense in depth (#259, AC-0285): dataset validation already
            // rejects interface-dispatch queries, so this reconstruction
            // re-derives the exclusion rather than trusting the dataset.
            exclusion: crate::is_interface_dispatch(&case.hop.query)
                .then_some(crate::HopExclusion::DynamicDispatch),
        };
        let candidates: Vec<_> = case
            .candidates
            .iter()
            .map(|c| crate::Candidate {
                node_id: c.node_id.clone(),
                kind: c.family.kind(),
                subtype: c.family.subtype().map(str::to_owned),
                text: c.text.clone(),
                evidence: evidence(&c.evidence),
                materialized_node: None,
            })
            .collect();
        let ranked = crate::propose(provider, &[hop], &candidates, retrieve)
            .map_err(|error| error.to_string())?;
        if ranked.iter().any(|p| {
            p.family() != &case.family
                || !p.similarity.is_finite()
                || !(0.0..=1.0).contains(&p.similarity)
        }) {
            return Err("invalid production proposal score or family".into());
        }
        let choice = choose(&ranked, family);
        predictions.push(CasePrediction {
            case: case.clone(),
            ranked,
            choice,
        });
    }
    check_binding()?;
    let records:Vec<_> = predictions.iter().map(|p|serde_json::json!({"case_id":p.case.case_id,"candidate_set":p.case.candidate_set_hash,"ranked":p.ranked.iter().map(|r|(&r.target_id,r.similarity.to_bits())).collect::<Vec<_>>(),"choice":p.choice})).collect();
    let digest = digest(
        &serde_json::json!({"recipe":"t2-sealed-production-predictions-bits@1","policy":policy.digest,"binding":inputs.binding,"records":records}),
    )?;
    Ok(SealedPredictions {
        policy: policy.clone(),
        predictions,
        digest,
    })
}
