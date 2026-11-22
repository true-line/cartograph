//! Host admission on a locked-runner receipt (ADR-0082 decision 7, AC-1511).
//! The host never holds evaluation labels. It admits a family only on a
//! receipt whose Ed25519 signature verifies against a key it pinned itself,
//! and only while the receipt still matches live state: the host's own family
//! parameters, its sealed embedding space and its own observation of the
//! selected corpora. Ranking and choosing are shared with the in-process
//! reviewed path ([`super::reviewed`]), so both bound, rank and choose alike.
use super::{
    locked::RECEIPT_PURPOSE,
    policy::{DatasetBinding, FamilyPolicy, FrozenPolicy},
    report::QualityMetrics,
    reviewed::{self, FamilyVerdict, LiveAdmission, LiveChecks, ObservedCorpus},
    scoring::ScoringCounts,
};
use crate::{Candidate, FamilyKey, UnresolvedHop, embedding_cache::CachedProvider};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Largest signed receipt document the host reads.
pub const MAX_SIGNED_BYTES: usize = 256 * 1024;
/// Most families, and most corpora, one receipt may carry.
const MAX_ENTRIES: usize = 256;

/// Every way a signed receipt is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptError {
    /// The document is not one closed, bounded signed receipt.
    Malformed,
    /// The pinned key is not 32 bytes of lowercase hex.
    InvalidPinnedKey,
    /// The embedded key is not the pinned key, or the signature fails.
    BadSignature,
}

/// What the runner prints: `eval_runner::Signed`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Signed {
    receipt: String,
    signature: String,
    public_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptCorpus {
    corpus_id: String,
    repo_id: String,
    revision: String,
    graph_snapshot_hash: String,
    capture_manifest_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptFamily {
    family: FamilyKey,
    adequate: bool,
    passed: bool,
    #[allow(dead_code, reason = "closed parse; counts are display-only")]
    counts: ScoringCounts,
    metrics: QualityMetrics,
}

/// The receipt the locked runner signs ([`super::locked::score`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptDocument {
    schema_version: u32,
    purpose: String,
    runner_version: String,
    envelope_sha256: String,
    dataset_id: String,
    inputs_hash: String,
    leakage_hash: String,
    labels_hash: String,
    corpora: Vec<ReceiptCorpus>,
    embedding_namespace: String,
    embedding_dimensions: u64,
    policy_digest: String,
    prediction_digest: String,
    receipt_digest: String,
    scored_at: u64,
    submission: u64,
    families: Vec<ReceiptFamily>,
}

fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn text(s: &str) -> bool {
    !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control)
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2)
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

impl ReceiptDocument {
    fn valid(&self) -> bool {
        let mut families = BTreeSet::new();
        let mut corpora = BTreeSet::new();
        self.schema_version == 1
            && self.purpose == RECEIPT_PURPOSE
            && text(&self.runner_version)
            && text(&self.dataset_id)
            && text(&self.embedding_namespace)
            && (1..=65_536).contains(&self.embedding_dimensions)
            && [
                &self.envelope_sha256,
                &self.inputs_hash,
                &self.leakage_hash,
                &self.labels_hash,
                &self.policy_digest,
                &self.prediction_digest,
                &self.receipt_digest,
            ]
            .iter()
            .all(|h| hash(h))
            && self.submission >= 1
            && !self.corpora.is_empty()
            && self.corpora.len() <= MAX_ENTRIES
            && self.corpora.iter().all(|c| {
                text(&c.corpus_id)
                    && text(&c.repo_id)
                    && text(&c.revision)
                    && hash(&c.graph_snapshot_hash)
                    && hash(&c.capture_manifest_hash)
                    && corpora.insert(c.corpus_id.as_str())
            })
            && !self.families.is_empty()
            && self.families.len() <= MAX_ENTRIES
            && self.families.iter().all(|f| {
                f.family.subtype().is_none_or(|s| s.len() <= 128)
                    && (f.adequate || !f.passed)
                    && f.metrics.precision.is_finite()
                    && f.metrics.recall.is_finite()
                    && families.insert(&f.family)
            })
    }

    /// The same corpora, in any order: the host lists its selectors in its
    /// own order, the receipt in the dataset's.
    fn corpora_match(&self, observed: &[ObservedCorpus]) -> bool {
        let mut receipt: Vec<_> = self
            .corpora
            .iter()
            .map(|c| {
                (
                    &c.corpus_id,
                    &c.repo_id,
                    &c.revision,
                    &c.graph_snapshot_hash,
                    &c.capture_manifest_hash,
                )
            })
            .collect();
        let mut live: Vec<_> = observed
            .iter()
            .map(|c| {
                (
                    &c.corpus_id,
                    &c.repo_id,
                    &c.revision,
                    &c.graph_snapshot_hash,
                    &c.capture_manifest_hash,
                )
            })
            .collect();
        receipt.sort();
        live.sort();
        receipt == live
    }
}

/// A receipt whose signature verified against the host's pinned key. It
/// grants nothing alone: [`VerifiedReceipt::admit_live`] revalidates it
/// against live state on every use.
#[derive(Debug, Clone)]
pub struct VerifiedReceipt {
    document: ReceiptDocument,
}

impl VerifiedReceipt {
    /// Verify the runner's signed output against `pinned_key_hex`, the key the
    /// host pinned at install time. The key embedded in the document is never
    /// trusted on its own.
    pub fn verify(signed: &[u8], pinned_key_hex: &str) -> Result<Self, ReceiptError> {
        let pinned = unhex(pinned_key_hex)
            .filter(|k| k.len() == 32)
            .ok_or(ReceiptError::InvalidPinnedKey)?;
        if signed.len() > MAX_SIGNED_BYTES {
            return Err(ReceiptError::Malformed);
        }
        let signed: Signed = serde_json::from_slice(signed).map_err(|_| ReceiptError::Malformed)?;
        let signature = unhex(&signed.signature)
            .filter(|s| s.len() == 64)
            .ok_or(ReceiptError::BadSignature)?;
        if signed.public_key != pinned_key_hex
            || UnparsedPublicKey::new(&ED25519, &pinned)
                .verify(signed.receipt.as_bytes(), &signature)
                .is_err()
        {
            return Err(ReceiptError::BadSignature);
        }
        // Parse only bytes the pinned key signed.
        let document: ReceiptDocument =
            serde_json::from_str(&signed.receipt).map_err(|_| ReceiptError::Malformed)?;
        if !document.valid() {
            return Err(ReceiptError::Malformed);
        }
        Ok(Self { document })
    }

    /// The runner's receipt digest, recorded on every family decision.
    pub fn receipt_digest(&self) -> &str {
        &self.document.receipt_digest
    }

    /// The policy digest the runner committed.
    pub fn policy_digest(&self) -> &str {
        &self.document.policy_digest
    }

    /// Millisecond time the runner scored the committed policy.
    pub fn scored_at(&self) -> u64 {
        self.document.scored_at
    }

    /// Rank live hops and admit per family on this receipt's verdicts. The
    /// host's own `parameters` must reproduce the committed policy digest
    /// over the receipt's dataset and the live sealed embedding space, and
    /// `observed` (the host's own observation of the selected corpora) must
    /// equal the receipt's corpora. Any mismatch abstains every family with
    /// `IncompatibleEvidence`.
    pub fn admit_live(
        &self,
        parameters: Vec<FamilyPolicy>,
        observed: &[ObservedCorpus],
        provider: &CachedProvider<'_>,
        hops: &[UnresolvedHop],
        candidates: &[Candidate],
    ) -> LiveAdmission {
        let d = &self.document;
        let families = || {
            hops.iter()
                .filter(|h| h.subtype.as_ref().is_none_or(|s| s.len() <= 128))
                .map(UnresolvedHop::family)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(MAX_ENTRIES)
        };
        let Some(live) = provider.finish_with_binding().ok().and_then(|(_, b)| b) else {
            return reviewed::abstained(families(), &d.receipt_digest);
        };
        let binding = DatasetBinding {
            dataset: d.dataset_id.clone(),
            purpose: "independent_evaluation".into(),
            inputs: d.inputs_hash.clone(),
            leakage: d.leakage_hash.clone(),
            labels: d.labels_hash.clone(),
        };
        let Ok(policy) = FrozenPolicy::for_locked_receipt(binding, &live, parameters) else {
            return reviewed::abstained(families(), &d.receipt_digest);
        };
        let verdicts: BTreeMap<_, _> = d
            .families
            .iter()
            .map(|f| {
                (
                    f.family.clone(),
                    FamilyVerdict {
                        adequate: f.adequate,
                        passed: f.passed,
                        metrics: f.metrics.clone(),
                    },
                )
            })
            .collect();
        let checks = LockedChecks {
            document: d,
            observed,
            policy: &policy,
            provider,
        };
        reviewed::rank_live(
            &checks,
            &verdicts,
            &d.receipt_digest,
            &policy,
            provider,
            hops,
            candidates,
        )
    }
}

struct LockedChecks<'a, 'p> {
    document: &'a ReceiptDocument,
    observed: &'a [ObservedCorpus],
    policy: &'a FrozenPolicy,
    provider: &'a CachedProvider<'p>,
}

impl LockedChecks<'_, '_> {
    fn space(&self) -> bool {
        self.provider
            .finish_with_binding()
            .ok()
            .and_then(|(_, b)| b)
            .is_some_and(|live| {
                &live == self.policy.embedding_binding()
                    && live.namespace() == self.document.embedding_namespace
                    && live.dimensions() as u64 == self.document.embedding_dimensions
            })
    }
}

impl LiveChecks for LockedChecks<'_, '_> {
    fn initial(&self) -> bool {
        self.policy.digest() == self.document.policy_digest
            && self.document.corpora_match(self.observed)
            && self.space()
    }
    fn after_family(&self) -> bool {
        self.space()
    }
    fn final_trust(&self) -> bool {
        self.initial()
    }
}

#[cfg(test)]
mod tests;
