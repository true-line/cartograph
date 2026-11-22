//! Locked-runner scoring (ADR-0082). The store sits in an account no agent can
//! read. One request carries a frozen family policy. The runner commits that
//! policy to the reviewed envelope once, seals predictions, scores them on the
//! held-out labels and returns a receipt of aggregate scores and digests only.
//! No case id, text, span, label or per-case outcome is in the receipt, and
//! every failure is one fixed code.
use super::{
    fixtures::{self, LoadedFixtureBundle},
    policy::{
        CalibrationView, EvaluationInputs, FamilyPolicy, FrozenPolicy, planned_embedding_texts,
    },
    report::QualityMetrics,
    reviewed::{ObservedCorpus, ReviewAnchor, ReviewEnvelope, ReviewedDataset},
    scoring::ScoringCounts,
};
use crate::{
    FamilyKey,
    embedding_cache::{CachedProvider, EmbeddingCache},
};
use llm::LlmProvider;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

/// Largest request the runner reads.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// Largest review record or commitment file the runner reads.
const MAX_RECORD_BYTES: u64 = 64 * 1024;
/// Purpose string of every receipt this module produces.
pub const RECEIPT_PURPOSE: &str = "locked_independent_receipt";

/// Every way a request can fail, as the fixed code the runner prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    /// The request is not one closed, bounded policy document.
    InvalidRequest,
    /// The store, its review record or its dataset is missing or malformed.
    InvalidStore,
    /// The reviewed dataset failed the ADR-0038 load checks.
    InvalidDataset,
    /// The embedding provider failed or its identity is not pinned.
    EmbeddingFailed,
    /// The family policy cannot be frozen against this calibration split.
    InvalidPolicy,
    /// A different policy was already committed for this dataset.
    PolicyAlreadyCommitted,
    /// Prediction or scoring failed.
    ScoringFailed,
    /// Reading or writing the store failed.
    StoreIo,
}

impl Code {
    /// The stable code text.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::InvalidRequest => "invalid_request",
            Code::InvalidStore => "invalid_store",
            Code::InvalidDataset => "invalid_dataset",
            Code::EmbeddingFailed => "embedding_failed",
            Code::InvalidPolicy => "invalid_policy",
            Code::PolicyAlreadyCommitted => "policy_already_committed",
            Code::ScoringFailed => "scoring_failed",
            Code::StoreIo => "store_io",
        }
    }
}

/// One family's operating point, as a request states it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyParameters {
    /// Exact `(HopKind, subtype)` family.
    pub family: FamilyKey,
    /// Similarity threshold.
    pub threshold: f32,
    /// Margin under which the two leaders abstain.
    pub ambiguity_margin: f32,
    /// Neighbours ranked per hop.
    pub neighbors: usize,
    /// Fewest evaluation cases for an adequate family.
    pub min_evaluation_cases: u64,
    /// Fewest answerable cases for an adequate family.
    pub min_answerable_cases: u64,
    /// Fewest no-match cases for an adequate family.
    pub min_no_match_cases: u64,
    /// Precision a family must reach to pass.
    pub precision_floor: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: u32,
    families: Vec<FamilyParameters>,
}

/// The review record the in-lock review step writes: who reviewed the labels,
/// who implements policy, and when the review closed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRecord {
    /// Always 1.
    pub schema_version: u32,
    /// Reviewer identity, recorded on every label.
    pub reviewer: String,
    /// Policy implementer identity, distinct from reviewer and every author.
    pub policy_implementer: String,
    /// Millisecond time the review closed.
    pub reviewed_at: u64,
}

/// The one policy committed for this store's dataset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Commitment {
    policy_digest: String,
    envelope_sha256: String,
}

#[derive(Serialize)]
struct FamilyResult<'a> {
    family: &'a FamilyKey,
    adequate: bool,
    passed: bool,
    counts: &'a ScoringCounts,
    metrics: QualityMetrics,
}

fn now() -> Result<u64, Code> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .ok_or(Code::StoreIo)
}

fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, Code> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Code::StoreIo),
    };
    if !meta.is_file() || meta.len() > MAX_RECORD_BYTES {
        return Err(Code::InvalidStore);
    }
    std::fs::read(path).map(Some).map_err(|_| Code::StoreIo)
}

/// Write a file whole: a temporary sibling, flushed, then renamed over.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Code> {
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|_| Code::StoreIo)?;
    file.write_all(bytes).map_err(|_| Code::StoreIo)?;
    file.sync_all().map_err(|_| Code::StoreIo)?;
    std::fs::rename(&tmp, path).map_err(|_| Code::StoreIo)
}

fn parse_request(bytes: &[u8]) -> Result<Vec<FamilyPolicy>, Code> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(Code::InvalidRequest);
    }
    let request: Request = serde_json::from_slice(bytes).map_err(|_| Code::InvalidRequest)?;
    if request.schema_version != 1 || request.families.is_empty() || request.families.len() > 256 {
        return Err(Code::InvalidRequest);
    }
    let mut seen = BTreeSet::new();
    request
        .families
        .into_iter()
        .map(|p| {
            let unit = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
            let valid = unit(p.threshold.into())
                && unit(p.ambiguity_margin.into())
                && unit(p.precision_floor)
                && (2..=256).contains(&p.neighbors)
                && [
                    p.min_evaluation_cases,
                    p.min_answerable_cases,
                    p.min_no_match_cases,
                ]
                .iter()
                .all(|n| (1..=1_000_000).contains(n))
                && p.family.subtype().is_none_or(|s| s.len() <= 128)
                && seen.insert(p.family.clone());
            valid
                .then_some(FamilyPolicy {
                    family: p.family,
                    threshold: p.threshold,
                    ambiguity_margin: p.ambiguity_margin,
                    neighbors: p.neighbors,
                    min_evaluation_cases: p.min_evaluation_cases,
                    min_answerable_cases: p.min_answerable_cases,
                    min_no_match_cases: p.min_no_match_cases,
                    precision_floor: p.precision_floor,
                })
                .ok_or(Code::InvalidRequest)
        })
        .collect()
}

/// The corpus identities the store's frozen corpus documents hash to. In the
/// locked store those documents are the retained capture itself.
pub(super) fn observed(
    dataset: &Path,
    bundle: &LoadedFixtureBundle,
) -> Result<Vec<ObservedCorpus>, Code> {
    bundle
        .inputs()
        .corpora
        .iter()
        .map(|c| {
            let read = |suffix: &str| {
                std::fs::read(dataset.join(format!("corpora/{}.{suffix}.json", c.corpus_id)))
                    .map(|bytes| fixtures::sha256(&bytes))
                    .map_err(|_| Code::InvalidStore)
            };
            Ok(ObservedCorpus {
                corpus_id: c.corpus_id.clone(),
                repo_id: c.repo_id.clone(),
                revision: c.revision.clone(),
                graph_snapshot_hash: read("graph")?,
                capture_manifest_hash: read("capture")?,
            })
        })
        .collect()
}

fn log(store: &Path, entry: serde_json::Value) -> Result<(), Code> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store.join("submissions.jsonl"))
        .map_err(|_| Code::StoreIo)?;
    let mut line = serde_json::to_vec(&entry).map_err(|_| Code::StoreIo)?;
    line.push(b'\n');
    file.write_all(&line).map_err(|_| Code::StoreIo)?;
    file.sync_all().map_err(|_| Code::StoreIo)
}

fn submissions(store: &Path) -> Result<u64, Code> {
    match std::fs::read(store.join("submissions.jsonl")) {
        Ok(bytes) => Ok(bytes.iter().filter(|b| **b == b'\n').count() as u64),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(_) => Err(Code::StoreIo),
    }
}

/// Score one request against the store at `store`, embedding through
/// `provider`. Returns the canonical receipt bytes the caller signs.
///
/// Store layout: `review.json` ([`ReviewRecord`]), `dataset/` (an
/// `independent_evaluation` bundle with its `corpora/`), and what the runner
/// writes itself: `cache.db`, `dataset/review-envelope.json`,
/// `commitment.json`, `receipt.json` and the append-only `submissions.jsonl`.
pub fn score(store: &Path, request: &[u8], provider: &dyn LlmProvider) -> Result<Vec<u8>, Code> {
    // Concurrent sessions are serialized from the commitment check through the
    // receipt and the log entry, so two policies can never both be committed.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(store.join("runner.lock"))
        .map_err(|_| Code::StoreIo)?;
    lock.lock().map_err(|_| Code::StoreIo)?;
    let at = now()?;
    let result = score_inner(store, request, provider, at);
    let outcome = match &result {
        Ok((_, digest)) => {
            serde_json::json!({"at": at, "outcome": "scored", "policy_digest": digest})
        }
        Err((code, digest)) => {
            serde_json::json!({"at": at, "outcome": code.as_str(), "policy_digest": digest})
        }
    };
    log(store, outcome)?;
    result.map(|(bytes, _)| bytes).map_err(|(code, _)| code)
}

type Scored = Result<(Vec<u8>, Option<String>), (Code, Option<String>)>;

fn score_inner(store: &Path, request: &[u8], provider: &dyn LlmProvider, at: u64) -> Scored {
    let bare = |code| (code, None);
    let parameters = parse_request(request).map_err(bare)?;
    let review: ReviewRecord = read_bounded(&store.join("review.json"))
        .map_err(bare)?
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|r: &ReviewRecord| r.schema_version == 1 && r.reviewed_at < at)
        .ok_or(bare(Code::InvalidStore))?;
    let dataset = store.join("dataset");
    let bundle =
        fixtures::load_independent_dataset(&dataset).map_err(|_| bare(Code::InvalidDataset))?;
    let observed = observed(&dataset, &bundle).map_err(bare)?;

    // Embed with the runner's own cache, keyed to this dataset.
    let mut cache =
        EmbeddingCache::open(store.join("cache.db")).map_err(|_| bare(Code::StoreIo))?;
    let instance = &fixtures::sha256(bundle.inputs().dataset_id.as_bytes())[..32];
    let lease = cache
        .bind_instance(instance)
        .map_err(|_| bare(Code::StoreIo))?;
    let cached = CachedProvider::new(provider, Arc::new(Mutex::new(cache)), lease)
        .map_err(|_| bare(Code::EmbeddingFailed))?;
    let calibration = CalibrationView::from_bundle(&bundle);
    let inputs = EvaluationInputs::from_bundle(&bundle);
    let texts =
        planned_embedding_texts(&calibration, &inputs).map_err(|_| bare(Code::InvalidDataset))?;
    cached
        .embed(&texts)
        .map_err(|_| bare(Code::EmbeddingFailed))?;
    cached.seal().map_err(|_| bare(Code::EmbeddingFailed))?;
    let binding = cached
        .finish_with_binding()
        .ok()
        .and_then(|(_, binding)| binding)
        .ok_or(bare(Code::EmbeddingFailed))?;
    let policy = FrozenPolicy::freeze(&calibration, &binding, parameters)
        .map_err(|_| bare(Code::InvalidPolicy))?;
    let digest = policy.digest().to_owned();
    let tagged = |code| (code, Some(digest.clone()));

    // One policy per dataset: commit it on first use, replay it after.
    let commitment_path = store.join("commitment.json");
    let receipt_path = store.join("receipt.json");
    let envelope_path = dataset.join("review-envelope.json");
    match read_bounded(&commitment_path).map_err(tagged)? {
        Some(bytes) => {
            let committed: Commitment =
                serde_json::from_slice(&bytes).map_err(|_| tagged(Code::InvalidStore))?;
            if committed.policy_digest != digest {
                return Err(tagged(Code::PolicyAlreadyCommitted));
            }
            if let Some(receipt) = read_bounded(&receipt_path).map_err(tagged)? {
                return Ok((receipt, Some(digest)));
            }
        }
        None => {
            let committed_at = at.checked_sub(1).ok_or(tagged(Code::StoreIo))?;
            if review.reviewed_at > committed_at {
                return Err(tagged(Code::InvalidStore));
            }
            let envelope = ReviewEnvelope {
                schema_version: 1,
                purpose: "reviewed_independent_dataset".into(),
                dataset_id: bundle.inputs().dataset_id.clone(),
                inputs_hash: bundle.digests().inputs.clone(),
                leakage_hash: bundle.digests().leakage.clone(),
                labels_hash: bundle.digests().labels.clone(),
                corpora_hash: fixtures::sha256(
                    &fixtures::canonical_json(&bundle.inputs().corpora)
                        .map_err(|_| tagged(Code::InvalidDataset))?,
                ),
                embedding_namespace: binding.namespace().into(),
                embedding_dimensions: binding.dimensions() as u64,
                policy_digest: digest.clone(),
                reviewer: review.reviewer.clone(),
                policy_implementer: review.policy_implementer.clone(),
                reviewed_at: review.reviewed_at,
                policy_committed_at: committed_at,
                calibration_started_at: at,
            };
            let bytes =
                fixtures::canonical_json(&envelope).map_err(|_| tagged(Code::InvalidStore))?;
            write_atomic(&envelope_path, &bytes).map_err(tagged)?;
            let commitment = Commitment {
                policy_digest: digest.clone(),
                envelope_sha256: fixtures::sha256(&bytes),
            };
            write_atomic(
                &commitment_path,
                &serde_json::to_vec(&commitment).map_err(|_| tagged(Code::StoreIo))?,
            )
            .map_err(tagged)?;
        }
    }
    let envelope = read_bounded(&envelope_path)
        .map_err(tagged)?
        .ok_or(tagged(Code::InvalidStore))?;
    let envelope_sha256 = fixtures::sha256(&envelope);
    let anchor = ReviewAnchor::new(envelope_sha256.clone(), review.policy_implementer.clone())
        .map_err(|_| tagged(Code::InvalidStore))?;
    let reviewed = ReviewedDataset::load(&dataset, &anchor, &observed)
        .map_err(|_| tagged(Code::InvalidDataset))?;
    let receipt = reviewed
        .predict_and_score(&policy, &cached)
        .map_err(|_| tagged(Code::ScoringFailed))?;
    let families: Vec<_> = receipt
        .scores()
        .iter()
        .map(|s| FamilyResult {
            family: s.family(),
            adequate: s.adequate(),
            passed: s.passed(),
            counts: s.counts(),
            metrics: s.metrics(),
        })
        .collect();
    let document = serde_json::json!({
        "schema_version": 1,
        "purpose": RECEIPT_PURPOSE,
        "runner_version": env!("CARGO_PKG_VERSION"),
        "envelope_sha256": envelope_sha256,
        "dataset_id": bundle.inputs().dataset_id,
        "inputs_hash": bundle.digests().inputs,
        "leakage_hash": bundle.digests().leakage,
        "labels_hash": bundle.digests().labels,
        "corpora": observed,
        "embedding_namespace": binding.namespace(),
        "embedding_dimensions": binding.dimensions(),
        "policy_digest": digest,
        "prediction_digest": receipt.prediction_digest(),
        "receipt_digest": receipt.digest(),
        "scored_at": at,
        "submission": submissions(store).map_err(tagged)? + 1,
        "families": families,
    });
    let bytes = serde_json::to_vec(&document).map_err(|_| tagged(Code::StoreIo))?;
    write_atomic(&receipt_path, &bytes).map_err(tagged)?;
    Ok((bytes, Some(digest)))
}

#[cfg(test)]
mod tests;
