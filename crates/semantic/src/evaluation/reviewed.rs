//! Host-reviewed independent dataset admission. A digest binds bytes, not label
//! truth or human identity. The application owns the root, anchor and observed
//! corpus evidence; none of them may come from an IPC caller's authority flags.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::{
    fixtures::{self, Corpus, LoadedFixtureBundle},
    policy::{self, CalibrationView, EvaluationInputs, FrozenPolicy, PredictionChoice},
    report::{DecisionReason, EvaluationSummary, FamilyDecision, FamilyStatus, SummaryMode},
    scoring::{self, EvaluationLabels, FamilyScore},
};
use crate::{
    Candidate, FamilyKey, SemanticProposal, UnresolvedHop, embedding_cache::CachedProvider,
};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
const MAX_TIME: u64 = 9_007_199_254_740_991;
/// Most live hops, and most live candidates, one admission ranks: the plan
/// bound the embedding cache admits.
const LIVE_INPUT_BOUND: usize = crate::embedding_cache::MAX_ROWS;
/// Most ranked rows one admission stages across its families: VSCode's
/// ~100k call hops at three neighbours each, with room to spare.
const LIVE_STAGED_BOUND: usize = 4 * crate::embedding_cache::MAX_ROWS;
type Result<T> = std::result::Result<T, String>;
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identity(s: &str) -> bool {
    !s.trim().is_empty() && s.trim() == s && s.len() <= 256 && !s.chars().any(char::is_control)
}
fn same_actor(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}
fn now() -> Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "host clock before epoch")?
            .as_millis(),
    )
    .map_err(|_| "host clock exceeds bound".into())
}
/// Host-observed fields, recomputed from captured bytes and graph snapshot.
/// Not Deserialize: a document echo is not evidence of current corpus identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservedCorpus {
    pub corpus_id: String,
    pub repo_id: String,
    pub revision: String,
    pub graph_snapshot_hash: String,
    pub capture_manifest_hash: String,
}
/// Host-owned review anchor, never a caller-supplied serialized grant.
#[derive(Debug, Clone)]
pub struct ReviewAnchor {
    envelope_hash: String,
    policy_implementer: String,
}
impl ReviewAnchor {
    pub fn new(expected_envelope_sha256: String, policy_implementer: String) -> Result<Self> {
        if !hash(&expected_envelope_sha256) || !identity(&policy_implementer) {
            return Err("invalid host review anchor".into());
        }
        Ok(Self {
            envelope_hash: expected_envelope_sha256,
            policy_implementer,
        })
    }
}
/// A closed descriptive envelope. Decoding it alone confers no authority.
/// Millisecond timestamps and independent authentication/order come from the
/// trusted host's review workflow; hashes cannot authenticate the named actors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewEnvelope {
    pub schema_version: u64,
    pub purpose: String,
    pub dataset_id: String,
    pub inputs_hash: String,
    pub leakage_hash: String,
    pub labels_hash: String,
    pub corpora_hash: String,
    pub embedding_namespace: String,
    pub embedding_dimensions: u64,
    pub policy_digest: String,
    pub reviewer: String,
    pub policy_implementer: String,
    pub reviewed_at: u64,
    pub policy_committed_at: u64,
    pub calibration_started_at: u64,
}
/// Constructed only after rooted loading, exact host anchor and corpus checks.
#[derive(Debug)]
pub struct ReviewedDataset {
    bundle: LoadedFixtureBundle,
    envelope: ReviewEnvelope,
    envelope_hash: String,
}
fn envelope(root: &Path) -> Result<ReviewEnvelope> {
    let root =
        Dir::open_ambient_dir(root, cap_std::ambient_authority()).map_err(|e| e.to_string())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut f = root
        .open_with("review-envelope.json", &options)
        .map_err(|e| e.to_string())?;
    let meta = f.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > MAX_ENVELOPE_BYTES as u64 {
        return Err("invalid reviewed envelope file bound".into());
    }
    let mut bytes = Vec::new();
    f.by_ref()
        .take(MAX_ENVELOPE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return Err("review envelope exceeds byte bound".into());
    }
    // Flat closed DTO: derive rejects escaped/duplicate field keys, unknown keys,
    // nullable/missing scalars, non-integer timestamps and unsupported nested data.
    serde_json::from_slice(&bytes).map_err(|_| "invalid closed reviewed envelope".into())
}
fn observed_matches(corpora: &[Corpus], observed: &[ObservedCorpus]) -> bool {
    if corpora.len() != observed.len() || observed.len() > 32 {
        return false;
    }
    let map: BTreeMap<_, _> = observed.iter().map(|c| (c.corpus_id.as_str(), c)).collect();
    map.len() == observed.len()
        && corpora.iter().all(|c| {
            map.get(c.corpus_id.as_str()).is_some_and(|o| {
                o.repo_id == c.repo_id
                    && o.revision == c.revision
                    && o.graph_snapshot_hash == c.graph_snapshot_hash
                    && o.capture_manifest_hash == c.capture_manifest_hash
            })
        })
}
impl ReviewedDataset {
    pub fn load(root: &Path, anchor: &ReviewAnchor, observed: &[ObservedCorpus]) -> Result<Self> {
        let bundle = fixtures::load_independent_dataset(root).map_err(|e| e.to_string())?;
        let envelope = envelope(root)?;
        let envelope_hash =
            fixtures::sha256(&fixtures::canonical_json(&envelope).map_err(|e| e.to_string())?);
        let d = bundle.digests();
        let current = now()?;
        if envelope.schema_version != 1
            || envelope.purpose != "reviewed_independent_dataset"
            || envelope_hash != anchor.envelope_hash
            || envelope.dataset_id != bundle.inputs().dataset_id
            || envelope.inputs_hash != d.inputs
            || envelope.leakage_hash != d.leakage
            || envelope.labels_hash != d.labels
            || envelope.corpora_hash
                != fixtures::sha256(
                    &fixtures::canonical_json(&bundle.inputs().corpora)
                        .map_err(|e| e.to_string())?,
                )
            || !hash(&envelope.embedding_namespace)
            || !hash(&envelope.policy_digest)
            || !(1..=4096).contains(&envelope.embedding_dimensions)
            || !identity(&envelope.reviewer)
            || !identity(&envelope.policy_implementer)
            || envelope.policy_implementer != anchor.policy_implementer
            || same_actor(&envelope.reviewer, &envelope.policy_implementer)
            || envelope.reviewed_at == 0
            || envelope.reviewed_at > envelope.policy_committed_at
            || envelope.policy_committed_at >= envelope.calibration_started_at
            || envelope.calibration_started_at > current
            || current > MAX_TIME
            || !observed_matches(bundle.unverified_corpus_references(), observed)
        {
            return Err("review envelope, chronology or host corpus binding mismatch".into());
        }
        for label in &bundle.labels().labels {
            if !identity(&label.origin.author)
                || same_actor(&envelope.reviewer, &label.origin.author)
                || same_actor(&envelope.policy_implementer, &label.origin.author)
                || label.origin.reviewer.as_deref() != Some(envelope.reviewer.as_str())
            {
                return Err("labels lack a distinct matching independent reviewer".into());
            }
        }
        Ok(Self {
            bundle,
            envelope,
            envelope_hash,
        })
    }
    /// Declared retained spans for host comparison with guarded current receipts.
    /// These references and bytes are fixture-consistent, not source authority.
    pub fn retained_sources(&self) -> &[fixtures::RetainedSource] {
        &self.bundle.inputs().sources
    }
    pub fn source_bytes(&self, id: &str) -> Option<&[u8]> {
        self.bundle.source_bytes(id)
    }
    pub fn calibration(&self) -> CalibrationView {
        CalibrationView::from_bundle(&self.bundle)
    }
    pub fn evaluation_inputs(&self) -> EvaluationInputs {
        EvaluationInputs::from_bundle(&self.bundle)
    }
    fn compatible_policy(
        &self,
        policy: &FrozenPolicy,
        provider: &CachedProvider<'_>,
    ) -> Result<()> {
        let binding = provider
            .finish_with_binding()
            .map_err(|e| e.to_string())?
            .1
            .ok_or("missing pinned embedding space")?;
        if policy.binding != policy::DatasetBinding::from_bundle(&self.bundle)
            || policy.digest() != self.envelope.policy_digest
            || policy.embedding_binding() != &binding
            || binding.namespace() != self.envelope.embedding_namespace
            || binding.dimensions() as u64 != self.envelope.embedding_dimensions
        {
            return Err("reviewed policy or embedding namespace/dimension mismatch".into());
        }
        Ok(())
    }
    pub fn predict_and_score(
        self,
        policy: &FrozenPolicy,
        provider: &CachedProvider<'_>,
    ) -> Result<IndependentReceipt> {
        self.compatible_policy(policy, provider)?;
        let prediction_started_at = now()?;
        if prediction_started_at < self.envelope.calibration_started_at {
            return Err("prediction precedes frozen policy/calibration chronology".into());
        }
        let sealed = policy::predict(policy, &self.evaluation_inputs(), provider)?;
        let scores =
            scoring::score_predictions(&sealed, &EvaluationLabels::from_bundle(&self.bundle))?;
        let scored_digest = scoring::scores_digest(&sealed, &scores, "independent_evaluation")?;
        self.compatible_policy(policy, provider)?;
        let digest = policy::digest(
            &serde_json::json!({"recipe":"t2-reviewed-independent-receipt@1","envelope":self.envelope_hash,"scored":scored_digest,"prediction_started_at":prediction_started_at}),
        )?;
        Ok(IndependentReceipt {
            dataset: self,
            sealed,
            scores,
            digest,
            prediction_started_at,
        })
    }
}
/// A separate receipt produced by actual sealed predictions plus shared scoring;
/// no Deserialize, unchecked public constructor or mechanism-receipt conversion.
#[derive(Debug)]
pub struct IndependentReceipt {
    dataset: ReviewedDataset,
    sealed: policy::SealedPredictions,
    scores: Vec<FamilyScore>,
    digest: String,
    prediction_started_at: u64,
}
#[derive(Debug)]
pub struct LiveAdmission {
    staged: Vec<SemanticProposal>,
    admitted: Vec<SemanticProposal>,
    summary: EvaluationSummary,
}
impl LiveAdmission {
    pub fn staged(&self) -> &[SemanticProposal] {
        &self.staged
    }
    pub fn admitted(&self) -> &[SemanticProposal] {
        &self.admitted
    }
    pub fn summary(&self) -> &EvaluationSummary {
        &self.summary
    }
    pub fn into_parts(
        self,
    ) -> (
        Vec<SemanticProposal>,
        Vec<SemanticProposal>,
        EvaluationSummary,
    ) {
        (self.staged, self.admitted, self.summary)
    }
    /// Preserve the same Gap-only overlay guard while attaching the actual
    /// independent receipt/family verdict, never a fabricated calibration report.
    pub fn overlay(
        &self,
        nodes: &[core_graph::Node],
        edges: &[core_graph::Edge],
    ) -> crate::Overlay {
        crate::overlay_with(nodes, edges, &self.admitted, |proposal| {
            let decision = self
                .summary
                .families
                .iter()
                .find(|f| &f.family == proposal.family());
            core_graph::Edge {
                src: proposal.source_id.clone(),
                dst: proposal.target_id.clone(),
                label: proposal.edge_label.clone(),
                props: serde_json::json!({
                 "resolver":crate::EXTRACTOR_ID,"fills_gap":proposal.gap_id,"similarity":proposal.similarity,
                 "eval":{"schema_version":1,"mode":"independent_evaluation","family":proposal.family(),"receipt_digest":decision.and_then(|f|f.receipt_digest.as_deref()),"decision":decision},
                 "prov":proposal.provenance,
                }),
            }
        })
    }
}
impl IndependentReceipt {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn scores(&self) -> &[FamilyScore] {
        &self.scores
    }
    pub fn policy_digest(&self) -> &str {
        self.sealed.policy_digest()
    }
    pub fn prediction_digest(&self) -> &str {
        self.sealed.digest()
    }
    #[allow(clippy::too_many_arguments)]
    pub fn predict_live(
        &self,
        root: &Path,
        anchor: &ReviewAnchor,
        observed: &[ObservedCorpus],
        policy: &FrozenPolicy,
        provider: &CachedProvider<'_>,
        hops: &[UnresolvedHop],
        candidates: &[Candidate],
    ) -> LiveAdmission {
        // The live inputs are bounded by the same plan bound the embedding
        // cache admits (AC-1507), so a VSCode-scale graph is ranked, not refused.
        if hops.len() > LIVE_INPUT_BOUND
            || candidates.len() > LIVE_INPUT_BOUND
            || hops
                .iter()
                .any(|h| h.subtype.as_ref().is_some_and(|s| s.len() > 128))
            || candidates
                .iter()
                .any(|c| c.subtype.as_ref().is_some_and(|s| s.len() > 128))
        {
            // Bound work before constructing groups or cloning live candidates.
            let families: BTreeSet<_> = hops
                .iter()
                .take(LIVE_INPUT_BOUND)
                .filter(|h| h.subtype.as_ref().is_none_or(|s| s.len() <= 128))
                .map(UnresolvedHop::family)
                .take(256)
                .collect();
            return LiveAdmission {
                staged: Vec::new(),
                admitted: Vec::new(),
                summary: EvaluationSummary {
                    schema_version: 1,
                    mode: SummaryMode::IndependentEvaluation,
                    families: families
                        .into_iter()
                        .map(|family| FamilyDecision {
                            family,
                            status: FamilyStatus::Abstained,
                            reason: DecisionReason::IncompatibleEvidence,
                            staged: 0,
                            admitted: 0,
                            ambiguous: 0,
                            receipt_digest: Some(self.digest.clone()),
                            metrics: None,
                        })
                        .collect(),
                },
            };
        }
        // Ranking happens here, not in a caller-supplied proposal list: every family
        // uses its frozen top-k and both leaders are present for ambiguity admission.
        let mut groups: BTreeMap<FamilyKey, Vec<&UnresolvedHop>> = BTreeMap::new();
        // Excluded hops (e.g. interface dispatch, #259) never stage here.
        for h in hops.iter().filter(|h| h.is_ann_candidate()) {
            groups.entry(h.family()).or_default().push(h);
        }
        // Ranking keeps `neighbors` rows per hop, so bound the staged rows the
        // families can produce, not only the inputs.
        let staged_bound = groups.iter().try_fold(0_usize, |sum, (family, group)| {
            let neighbors = policy.families.get(family).map_or(3, |p| p.neighbors);
            sum.checked_add(group.len().checked_mul(neighbors)?)
        });
        if groups.len() > 256 || staged_bound.is_none_or(|rows| rows > LIVE_STAGED_BOUND) {
            return LiveAdmission {
                staged: Vec::new(),
                admitted: Vec::new(),
                summary: EvaluationSummary {
                    schema_version: 1,
                    mode: SummaryMode::IndependentEvaluation,
                    families: groups
                        .into_keys()
                        .take(256)
                        .map(|family| FamilyDecision {
                            family,
                            status: FamilyStatus::Abstained,
                            reason: DecisionReason::IncompatibleEvidence,
                            staged: 0,
                            admitted: 0,
                            ambiguous: 0,
                            receipt_digest: Some(self.digest.clone()),
                            metrics: None,
                        })
                        .collect(),
                },
            };
        }
        let compatible = ReviewedDataset::load(root, anchor, observed)
            .and_then(|fresh| {
                if fresh.envelope_hash != self.dataset.envelope_hash
                    || fresh.bundle.digests() != self.dataset.bundle.digests()
                    || self.prediction_started_at < fresh.envelope.calibration_started_at
                    || policy.digest() != self.policy_digest()
                {
                    return Err("independent receipt no longer compatible".into());
                }
                fresh.compatible_policy(policy, provider)
            })
            .is_ok();
        let mut staged = Vec::new();
        let mut admitted = Vec::new();
        let mut families = Vec::new();

        // Even malformed duplicate gap identities cannot win through ordering.
        let mut gaps = BTreeSet::new();
        let unique_gaps = hops.iter().all(|h| gaps.insert(&h.gap_id));
        for (family, group) in groups {
            let p = policy.families.get(&family);
            let relevant: Vec<_> = candidates
                .iter()
                .filter(|c| c.family() == family)
                .cloned()
                .collect();
            let mut candidate_ids = BTreeSet::new();
            let unique_candidates = relevant.iter().all(|c| candidate_ids.insert(&c.node_id));
            let local_hops: Vec<_> = group.into_iter().cloned().collect();
            let ranked = if !unique_gaps || !unique_candidates {
                Err("live input bound or identity mismatch".into())
            } else {
                crate::propose(
                    provider,
                    &local_hops,
                    &relevant,
                    p.map(|p| p.neighbors).unwrap_or(3),
                )
                .map_err(|e| e.to_string())
            };
            let score = self.scores.iter().find(|s| s.family() == &family);
            let mut reason = if !compatible {
                DecisionReason::IncompatibleEvidence
            } else if p.is_none() || score.is_none() {
                DecisionReason::MissingEvidence
            } else if !score.is_some_and(FamilyScore::adequate) {
                DecisionReason::InsufficientEvidence
            } else if !score.is_some_and(FamilyScore::passed) {
                DecisionReason::BelowFloor
            } else {
                DecisionReason::IndependentlyEvaluated
            };
            let mut local_admitted = Vec::new();
            let mut ambiguous = 0;
            let local = match ranked {
                Ok(ranked) => ranked,
                Err(_) => {
                    reason = DecisionReason::IncompatibleEvidence;
                    Vec::new()
                }
            };
            if local.iter().any(|r| {
                r.family() != &family
                    || !r.similarity.is_finite()
                    || !(0.0..=1.0).contains(&r.similarity)
            }) {
                reason = DecisionReason::IncompatibleEvidence;
            }
            if let Some(p) = p {
                // One pass groups the ranked rows by Gap, keeping ranking order,
                // so choosing stays linear in the staged rows at scale.
                // Choosing reads only a Gap's two leaders, so only those are copied.
                let mut by_gap: BTreeMap<&str, Vec<&SemanticProposal>> = BTreeMap::new();
                for r in &local {
                    by_gap.entry(r.gap_id.as_str()).or_default().push(r);
                }
                for h in &local_hops {
                    let gap_rows = by_gap.remove(h.gap_id.as_str()).unwrap_or_default();
                    let rows: Vec<SemanticProposal> =
                        gap_rows.iter().take(2).map(|r| (*r).clone()).collect();
                    match policy::choose(&rows, p) {
                        PredictionChoice::Ambiguous => ambiguous += gap_rows.len() as u64,
                        PredictionChoice::Admitted { .. }
                            if reason == DecisionReason::IndependentlyEvaluated =>
                        {
                            if let Some(best) = rows.first() {
                                local_admitted.push(best.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Recheck the exact operation after ranking. A lease/model change cannot
            // retain grants or earlier family admissions.
            if self.dataset.compatible_policy(policy, provider).is_err() {
                reason = DecisionReason::IncompatibleEvidence;
                local_admitted.clear();
            }
            let status = if matches!(
                reason,
                DecisionReason::IndependentlyEvaluated | DecisionReason::BelowFloor
            ) {
                FamilyStatus::IndependentlyEvaluated
            } else {
                FamilyStatus::Abstained
            };
            families.push(FamilyDecision {
                family,
                status,
                reason,
                staged: local.len() as u64,
                admitted: local_admitted.len() as u64,
                ambiguous,
                receipt_digest: Some(self.digest.clone()),
                metrics: score.map(FamilyScore::metrics),
            });
            staged.extend(local);
            admitted.extend(local_admitted);
        }
        let final_trust = ReviewedDataset::load(root, anchor, observed)
            .and_then(|fresh| {
                if fresh.envelope_hash != self.dataset.envelope_hash
                    || fresh.bundle.digests() != self.dataset.bundle.digests()
                {
                    return Err("review changed during live prediction".into());
                }
                fresh.compatible_policy(policy, provider)
            })
            .is_ok();
        if !final_trust {
            admitted.clear();
            for f in &mut families {
                f.admitted = 0;
                f.status = FamilyStatus::Abstained;
                f.reason = DecisionReason::IncompatibleEvidence;
            }
        }
        let mut summary = EvaluationSummary {
            schema_version: 1,
            mode: SummaryMode::IndependentEvaluation,
            families,
        };
        if summary.validate().is_err() {
            admitted.clear();
            for f in &mut summary.families {
                f.status = FamilyStatus::Abstained;
                f.reason = DecisionReason::IncompatibleEvidence;
                f.admitted = 0;
            }
        }
        LiveAdmission {
            staged,
            admitted,
            summary,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        HopKind,
        embedding_cache::EmbeddingCache,
        evaluation::policy::{FamilyPolicy, planned_embedding_texts},
    };
    use llm::{Embedding, EmbeddingIdentity, LlmProvider, Locality, ProviderCaps, ProviderError};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    pub(crate) struct Scripted {
        pub(crate) calls: AtomicUsize,
        pub(crate) version: AtomicUsize,
    }
    impl LlmProvider for Scripted {
        fn id(&self) -> &str {
            "reviewed-scripted-only"
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
        fn embedding_cache_identity(
            &self,
        ) -> std::result::Result<Option<EmbeddingIdentity>, ProviderError> {
            Ok(Some(EmbeddingIdentity {
                endpoint: "scripted://reviewed".into(),
                model: "mechanism-not-quality".into(),
                digest: format!("sha256:{:064x}", self.version.load(Ordering::SeqCst)),
                recipe: "reviewed-mechanism-v1".into(),
            }))
        }
        fn embed(&self, texts: &[String]) -> std::result::Result<Vec<Embedding>, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(texts
                .iter()
                .map(|t| {
                    if t.contains("chargeCard") {
                        vec![1., 0., 0.]
                    } else if t.contains("refundCard") {
                        vec![0., 1., 0.]
                    } else if t.contains("formatCurrency") {
                        vec![0., 0., 1.]
                    } else {
                        vec![1., 1., 1.]
                    }
                })
                .collect())
        }
    }
    fn copy(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let e = entry.unwrap();
            let to = dst.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to);
            } else {
                std::fs::copy(e.path(), to).unwrap();
            }
        }
    }
    pub(crate) fn independent_files(missing_policy_case: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/t2-visible-mechanism-v1"),
            dir.path(),
        );
        let read = |name: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(dir.path().join(name)).unwrap()).unwrap()
        };
        let mut i = read("inputs.json");
        let mut l = read("leakage.json");
        let mut b = read("labels.json");
        for doc in [&mut i, &mut l, &mut b] {
            doc["purpose"] = serde_json::json!("independent_evaluation");
        }
        for label in b["labels"].as_array_mut().unwrap() {
            label["origin"]["reviewer"] = serde_json::json!("separate-reviewer");
        }
        if missing_policy_case {
            // Retain a genuine evaluation-split case with no configured policy,
            // alongside the adequately supported Call family.
            let family = serde_json::json!({"kind":"Channel", "subtype":"missing-policy"});
            let case = i["cases"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|case| case["case_id"] == "c-call-multi-acceptable")
                .unwrap();
            case["family"] = family.clone();
            case["hop"]["edge_label"] = serde_json::json!("PUBLISHES");
            for candidate in case["candidates"].as_array_mut().unwrap() {
                candidate["family"] = family.clone();
            }
            case["candidate_set_hash"] = serde_json::json!(fixtures::sha256(
                &fixtures::canonical_json(&case["candidates"]).unwrap()
            ));
        }
        fixtures::catalogue_test_documents(&mut i, &mut l, &mut b);
        let input = fixtures::canonical_json(&i).unwrap();
        l["inputs_hash"] = serde_json::json!(fixtures::sha256(&input));
        b["inputs_hash"] = l["inputs_hash"].clone();
        let leakage = fixtures::canonical_json(&l).unwrap();
        b["leakage_hash"] = serde_json::json!(fixtures::sha256(&leakage));
        for (name, bytes) in [
            ("inputs.json", input),
            ("leakage.json", leakage),
            ("labels.json", fixtures::canonical_json(&b).unwrap()),
        ] {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        dir
    }
    fn observed(root: &Path, b: &LoadedFixtureBundle) -> Vec<ObservedCorpus> {
        b.inputs()
            .corpora
            .iter()
            .map(|c| ObservedCorpus {
                corpus_id: c.corpus_id.clone(),
                repo_id: c.repo_id.clone(),
                revision: c.revision.clone(),
                graph_snapshot_hash: fixtures::sha256(
                    &std::fs::read(root.join(format!("corpora/{}.graph.json", c.corpus_id)))
                        .unwrap(),
                ),
                capture_manifest_hash: fixtures::sha256(
                    &std::fs::read(root.join(format!("corpora/{}.capture.json", c.corpus_id)))
                        .unwrap(),
                ),
            })
            .collect()
    }
    fn anchor(root: &Path, envelope: &ReviewEnvelope) -> ReviewAnchor {
        let bytes = fixtures::canonical_json(envelope).unwrap();
        std::fs::write(root.join("review-envelope.json"), &bytes).unwrap();
        ReviewAnchor::new(fixtures::sha256(&bytes), "policy-implementer".into()).unwrap()
    }
    fn live(b: &LoadedFixtureBundle, id: &str) -> (Vec<UnresolvedHop>, Vec<Candidate>) {
        let c = b.inputs().cases.iter().find(|c| c.case_id == id).unwrap();
        let ev = |es: &[fixtures::EvidenceRef]| {
            es.iter()
                .map(|e| core_prov::EvidenceRef {
                    repo: e.repo.clone(),
                    path: e.path.clone(),
                    byte_start: e.byte_start,
                    byte_end: e.byte_end,
                    commit_sha: e.commit_sha.clone(),
                })
                .collect()
        };
        (
            vec![UnresolvedHop {
                gap_id: c.hop.gap_id.clone(),
                source_id: c.hop.source_id.clone(),
                edge_label: match c.hop.edge_label {
                    fixtures::EdgeLabel::CALLS => "CALLS",
                    fixtures::EdgeLabel::PUBLISHES => "PUBLISHES",
                    fixtures::EdgeLabel::SUBSCRIBES => "SUBSCRIBES",
                }
                .into(),
                kind: c.family.kind(),
                subtype: c.family.subtype().map(str::to_owned),
                query: c.hop.query.clone(),
                evidence: ev(&c.hop.evidence),
                exclusion: None,
            }],
            c.candidates
                .iter()
                .map(|c| Candidate {
                    node_id: c.node_id.clone(),
                    kind: c.family.kind(),
                    subtype: c.family.subtype().map(str::to_owned),
                    text: c.text.clone(),
                    evidence: ev(&c.evidence),
                    materialized_node: None,
                })
                .collect(),
        )
    }
    // AC-0274/0277: synthetic host review records exercise actual sealed prediction,
    // scoring and live gate consumption. These are mechanism tests, not a claim
    // that renaming a visible fixture establishes an independently valid dataset.
    #[test]
    fn reviewed_receipt_scores_shared_predictions_and_live_gate_revalidates_trust() {
        reviewed_receipt_scenario(false);
    }

    // AC-0275: missing family policy abstains without disabling configured families.
    #[test]
    fn reviewed_missing_family_policy_preserves_configured_admission() {
        reviewed_receipt_scenario(true);
    }

    fn reviewed_receipt_scenario(omit_channel_policy: bool) {
        let dir = independent_files(omit_channel_policy);
        let bundle = fixtures::load_independent_dataset(dir.path()).unwrap();
        let observations = observed(dir.path(), &bundle);
        let provider = Scripted {
            calls: AtomicUsize::new(0),
            version: AtomicUsize::new(1),
        };
        let mut cache = EmbeddingCache::open(dir.path().join("cache.db")).unwrap();
        let lease = cache.bind_instance(&"a".repeat(32)).unwrap();
        let cached = CachedProvider::new(&provider, Arc::new(Mutex::new(cache)), lease).unwrap();
        let calibration = CalibrationView::from_bundle(&bundle);
        let inputs = EvaluationInputs::from_bundle(&bundle);
        cached
            .embed(&planned_embedding_texts(&calibration, &inputs).unwrap())
            .unwrap();
        cached.seal().unwrap();
        let binding = cached.finish_with_binding().unwrap().1.unwrap();
        let parameters: Vec<_> = bundle
            .inputs()
            .cases
            .iter()
            .map(|c| c.family.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|family| !omit_channel_policy || family.kind() == HopKind::Call)
            .map(|family| FamilyPolicy {
                family,
                threshold: 0.9,
                ambiguity_margin: 0.01,
                neighbors: 3,
                min_evaluation_cases: 1,
                min_answerable_cases: 1,
                min_no_match_cases: 1,
                precision_floor: 0.95,
            })
            .collect();
        let policy = FrozenPolicy::freeze(&calibration, &binding, parameters).unwrap();
        let t = now().unwrap() - 10;
        let envelope = ReviewEnvelope {
            schema_version: 1,
            purpose: "reviewed_independent_dataset".into(),
            dataset_id: bundle.inputs().dataset_id.clone(),
            inputs_hash: bundle.digests().inputs.clone(),
            leakage_hash: bundle.digests().leakage.clone(),
            labels_hash: bundle.digests().labels.clone(),
            corpora_hash: fixtures::sha256(
                &fixtures::canonical_json(&bundle.inputs().corpora).unwrap(),
            ),
            embedding_namespace: binding.namespace().into(),
            embedding_dimensions: binding.dimensions() as u64,
            policy_digest: policy.digest().into(),
            reviewer: "separate-reviewer".into(),
            policy_implementer: "policy-implementer".into(),
            reviewed_at: t - 2,
            policy_committed_at: t - 1,
            calibration_started_at: t,
        };
        let good_anchor = anchor(dir.path(), &envelope);
        let mut bad_observed = observations.clone();
        bad_observed[0].graph_snapshot_hash = "0".repeat(64);
        assert!(ReviewedDataset::load(dir.path(), &good_anchor, &bad_observed).is_err());
        let mut bad = envelope.clone();
        bad.policy_committed_at = bad.calibration_started_at + 1;
        let a = anchor(dir.path(), &bad);
        assert!(ReviewedDataset::load(dir.path(), &a, &observations).is_err());
        bad = envelope.clone();
        bad.reviewer = bundle.labels().labels[0].origin.author.clone();
        let a = anchor(dir.path(), &bad);
        assert!(ReviewedDataset::load(dir.path(), &a, &observations).is_err());
        anchor(dir.path(), &envelope);
        let reviewed = ReviewedDataset::load(dir.path(), &good_anchor, &observations).unwrap();
        // The public visible scorer cannot cast this purpose into MechanismReceipt.
        let sealed = policy::predict(&policy, &inputs, &cached).unwrap();
        if omit_channel_policy {
            assert!(
                sealed
                    .predictions
                    .iter()
                    .any(|prediction| prediction.choice == policy::PredictionChoice::MissingPolicy)
            );
            let mut missing = policy::predict(&policy, &inputs, &cached).unwrap();
            missing
                .predictions
                .retain(|prediction| prediction.choice != policy::PredictionChoice::MissingPolicy);
            assert!(
                scoring::score_predictions(&missing, &EvaluationLabels::from_bundle(&bundle))
                    .is_err()
            );
            let mut duplicate = policy::predict(&policy, &inputs, &cached).unwrap();
            duplicate.predictions.push(duplicate.predictions[0].clone());
            assert!(
                scoring::score_predictions(&duplicate, &EvaluationLabels::from_bundle(&bundle))
                    .is_err()
            );
        }
        assert!(scoring::score(sealed, EvaluationLabels::from_bundle(&bundle)).is_err());
        let receipt = reviewed.predict_and_score(&policy, &cached).unwrap();
        let call = receipt
            .scores()
            .iter()
            .find(|s| s.family() == &FamilyKey::new(HopKind::Call, None))
            .unwrap();
        assert!(call.passed());
        let calls = provider.calls.load(Ordering::SeqCst);
        let (hops, candidates) = live(&bundle, "c-call-unique");
        let result = receipt.predict_live(
            dir.path(),
            &good_anchor,
            &observations,
            &policy,
            &cached,
            &hops,
            &candidates,
        );
        assert_eq!(result.admitted.len(), 1);
        assert!(!result.staged.is_empty());
        if !omit_channel_policy {
            // AC-1509: past the former 4096-hop cap, every hop is still ranked
            // and admitted under the same receipt, with no new embedding calls.
            let many: Vec<_> = (0..5000)
                .map(|i| UnresolvedHop {
                    gap_id: format!("{}-{i}", hops[0].gap_id),
                    ..hops[0].clone()
                })
                .collect();
            let scaled = receipt.predict_live(
                dir.path(),
                &good_anchor,
                &observations,
                &policy,
                &cached,
                &many,
                &candidates,
            );
            assert_eq!(scaled.admitted.len(), 5000);
            assert_eq!(scaled.summary.families[0].admitted, 5000);
            assert_eq!(
                scaled.summary.families[0].reason,
                DecisionReason::IndependentlyEvaluated
            );
            assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
        }
        if omit_channel_policy {
            let (mut mixed_hops, mut mixed_candidates) = live(&bundle, "c-call-multi-acceptable");
            mixed_hops.extend(hops.clone());
            mixed_candidates.extend(candidates.clone());
            let mixed = receipt.predict_live(
                dir.path(),
                &good_anchor,
                &observations,
                &policy,
                &cached,
                &mixed_hops,
                &mixed_candidates,
            );
            assert_eq!(mixed.admitted.len(), 1);
            assert!(mixed.summary.families.iter().any(|family| family.family
                == FamilyKey::new(HopKind::Call, None)
                && family.reason == DecisionReason::IndependentlyEvaluated));
            assert!(mixed.summary.families.iter().any(|family| family.family
                == FamilyKey::new(HopKind::Channel, Some("missing-policy".into()))
                && family.reason == DecisionReason::MissingEvidence
                && family.admitted == 0));
        }
        assert_eq!(
            result.summary.families[0].reason,
            DecisionReason::IndependentlyEvaluated
        );
        let nodes = vec![
            core_graph::Node {
                id: hops[0].source_id.clone(),
                label: "Symbol".into(),
                props: serde_json::json!({"original":"confirmed"}),
            },
            core_graph::Node {
                id: hops[0].gap_id.clone(),
                label: "Gap".into(),
                props: serde_json::json!({"reason":"unresolved"}),
            },
            core_graph::Node {
                id: candidates[0].node_id.clone(),
                label: "Symbol".into(),
                props: serde_json::json!({"original":"target"}),
            },
        ];
        let gap_edge = core_graph::Edge {
            src: hops[0].source_id.clone(),
            dst: hops[0].gap_id.clone(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov":{"confidence_tier":"Gap"}}),
        };
        let original = core_graph::Edge {
            src: "confirmed-src".into(),
            dst: "confirmed-dst".into(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov":{"tier":"Deterministic","confidence_tier":"Confirmed"},"original":"unchanged"}),
        };
        let overlay = result.overlay(&nodes, &[gap_edge, original.clone()]);
        assert_eq!(overlay.gaps_filled, 1);
        assert!(overlay.edges.contains(&original));
        assert!(overlay.nodes.contains(&nodes[0]));
        let inferred = overlay
            .edges
            .iter()
            .find(|e| e.props["fills_gap"] == hops[0].gap_id)
            .unwrap();
        assert_eq!(inferred.props["eval"]["receipt_digest"], receipt.digest());
        assert_eq!(
            inferred.props["eval"]["family"],
            serde_json::to_value(FamilyKey::new(HopKind::Call, None)).unwrap()
        );
        assert_eq!(inferred.props["prov"]["tier"], "Semantic");
        assert_eq!(inferred.props["prov"]["confidence_tier"], "InferredStrong");
        let (ambiguous_hops, ambiguous_candidates) = live(&bundle, "c-call-same-name");
        let ambiguous = receipt.predict_live(
            dir.path(),
            &good_anchor,
            &observations,
            &policy,
            &cached,
            &ambiguous_hops,
            &ambiguous_candidates,
        );
        assert!(ambiguous.admitted.is_empty());
        assert!(ambiguous.summary.families[0].ambiguous > 0);
        let (ch, cc) = live(&bundle, "c-channel-sqs");
        let thin = receipt.predict_live(
            dir.path(),
            &good_anchor,
            &observations,
            &policy,
            &cached,
            &ch,
            &cc,
        );
        assert!(thin.admitted.is_empty());
        assert_eq!(
            thin.summary.families[0].reason,
            if omit_channel_policy {
                DecisionReason::MissingEvidence
            } else {
                DecisionReason::InsufficientEvidence
            }
        );
        let changed = receipt.predict_live(
            dir.path(),
            &good_anchor,
            &bad_observed,
            &policy,
            &cached,
            &hops,
            &candidates,
        );
        assert!(changed.admitted.is_empty());
        assert!(!changed.staged.is_empty());
        assert_eq!(
            changed.summary.families[0].reason,
            DecisionReason::IncompatibleEvidence
        );
        bad = envelope.clone();
        bad.embedding_dimensions += 1;
        let a = anchor(dir.path(), &bad);
        let wrong_dimension = receipt.predict_live(
            dir.path(),
            &a,
            &observations,
            &policy,
            &cached,
            &hops,
            &candidates,
        );
        assert!(wrong_dimension.admitted.is_empty());
        assert_eq!(
            wrong_dimension.summary.families[0].reason,
            DecisionReason::IncompatibleEvidence
        );
        anchor(dir.path(), &envelope);
        std::fs::write(dir.path().join("labels.json"), b"{}").unwrap();
        let changed = receipt.predict_live(
            dir.path(),
            &good_anchor,
            &observations,
            &policy,
            &cached,
            &hops,
            &candidates,
        );
        assert!(changed.admitted.is_empty());
        assert!(!changed.staged.is_empty());
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            calls,
            "sealed production ranking performs no new embedding calls"
        );
    }
}
