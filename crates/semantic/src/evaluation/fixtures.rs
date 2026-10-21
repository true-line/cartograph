//! Frozen visible mechanism fixture consistency validation. Never establishes label truth.
use crate::FamilyKey;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
mod canonical;
mod filesystem;
#[cfg(test)]
mod tests;
mod validation;
pub use canonical::{canonical_json, sha256};
pub const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_BUNDLE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 65536;
#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("invalid visible fixture: {0}")]
    Invalid(&'static str),
    #[error("visible fixture capacity exceeded")]
    Capacity,
    #[error("visible fixture I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
type Result<T> = std::result::Result<T, FixtureError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub repo: String,
    pub path: String,
    pub byte_start: u64,
    pub byte_end: u64,
    pub commit_sha: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedSource {
    pub source_id: String,
    pub corpus_id: String,
    pub evidence: EvidenceRef,
    pub bytes_path: String,
    pub byte_len: u64,
    pub content_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub node_id: String,
    pub family: FamilyKey,
    pub text: String,
    pub text_hash: String,
    pub evidence: Vec<EvidenceRef>,
    pub source_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseInput {
    pub case_id: String,
    pub corpus_id: String,
    pub family: FamilyKey,
    pub hop: Hop,
    pub source_ids: Vec<String>,
    pub candidates: Vec<Candidate>,
    pub candidate_set_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelOrigin {
    pub kind: OriginKind,
    pub author: String,
    pub reviewer: Option<String>,
    pub procedure_version: String,
    pub source_ids: Vec<String>,
    pub rationale: String,
    pub exclusivity_basis: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseLabel {
    pub case_id: String,
    pub truth: Truth,
    pub hard_negative_target_ids: Vec<String>,
    pub origin: LabelOrigin,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputsManifest {
    pub schema_version: u64,
    pub kind: String,
    pub purpose: String,
    pub dataset_id: String,
    pub corpora: Vec<Corpus>,
    pub sources: Vec<RetainedSource>,
    pub cases: Vec<CaseInput>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeakageManifest {
    pub schema_version: u64,
    pub kind: String,
    pub purpose: String,
    pub dataset_id: String,
    pub inputs_hash: String,
    pub partition_policy: PartitionPolicy,
    pub groups: Vec<LeakageGroup>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelsManifest {
    pub schema_version: u64,
    pub kind: String,
    pub purpose: String,
    pub dataset_id: String,
    pub inputs_hash: String,
    pub leakage_hash: String,
    pub labels: Vec<CaseLabel>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Corpus {
    pub corpus_id: String,
    pub repo_id: String,
    pub revision: String,
    pub graph_snapshot_hash: String,
    pub capture_manifest_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hop {
    pub gap_id: String,
    pub source_id: String,
    pub edge_label: EdgeLabel,
    pub query: String,
    pub query_hash: String,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionPolicy {
    pub version: String,
    pub salt: String,
    pub calibration_buckets: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeakageGroup {
    pub group_id: String,
    pub case_ids: Vec<String>,
    pub split: Split,
    pub related_source_ids: Vec<String>,
    pub grouping_basis: Vec<GroupingBasis>,
    pub review_note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Truth {
    AcceptableTargets { target_ids: Vec<String> },
    NoMatch,
    Unknown { reason: String },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeLabel {
    CALLS,
    PUBLISHES,
    SUBSCRIBES,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    ScriptedTruth,
    SourceProof,
    IndependentAdjudication,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Calibration,
    Evaluation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupingBasis {
    SameSource,
    SameTargetDefinition,
    SameRevisionLineage,
    DuplicateText,
    NearDuplicateText,
    Independent,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureDigests {
    pub inputs: String,
    pub leakage: String,
    pub labels: String,
}
/// Private construction: untrusted DTOs alone never confer gate authority.
#[derive(Debug)]
pub struct LoadedFixtureBundle {
    inputs: InputsManifest,
    leakage: LeakageManifest,
    labels: LabelsManifest,
    digests: FixtureDigests,
    sources: BTreeMap<String, Vec<u8>>,
}
#[derive(Clone, Copy)]
enum ExpectedPurpose {
    VisibleMechanism,
    IndependentEvaluation,
}
impl ExpectedPurpose {
    fn value(self) -> &'static str {
        match self {
            Self::VisibleMechanism => "visible_mechanism_fixture",
            Self::IndependentEvaluation => "independent_evaluation",
        }
    }
}
impl LoadedFixtureBundle {
    /// Actual checked purpose; consistency alone is not evaluation authority.
    pub fn purpose(&self) -> &str {
        &self.inputs.purpose
    }

    /// Declared graph/capture digests only. This loader has no graph/capture
    /// document paths in the frozen format and does not establish their bytes.
    /// A production host must independently match observed corpus bindings.
    pub fn unverified_corpus_references(&self) -> &[Corpus] {
        &self.inputs.corpora
    }
    pub fn inputs(&self) -> &InputsManifest {
        &self.inputs
    }
    pub fn leakage(&self) -> &LeakageManifest {
        &self.leakage
    }
    pub fn labels(&self) -> &LabelsManifest {
        &self.labels
    }
    pub fn digests(&self) -> &FixtureDigests {
        &self.digests
    }
    pub fn source_bytes(&self, id: &str) -> Option<&[u8]> {
        self.sources.get(id).map(Vec::as_slice)
    }
}
pub fn load_fixture_bundle(root: &Path) -> Result<LoadedFixtureBundle> {
    filesystem::load(root, ExpectedPurpose::VisibleMechanism)
}

/// Consistency-only dataset loading for the host's reviewed trust boundary.
/// This cannot promote a visible fixture or authenticate an independent review.
pub(crate) fn load_independent_dataset(root: &Path) -> Result<LoadedFixtureBundle> {
    filesystem::load(root, ExpectedPurpose::IndependentEvaluation)
}

/// Test-only independent-rule conversion of already-authored visible mechanism
/// documents. No labels, group assignments, salt or declared splits are changed.
#[cfg(test)]
pub(crate) fn catalogue_test_documents(
    i: &mut serde_json::Value,
    l: &mut serde_json::Value,
    b: &mut serde_json::Value,
) {
    use serde_json::json;
    use std::collections::BTreeSet;
    for doc in [&mut *i, &mut *l, &mut *b] {
        doc["purpose"] = json!("independent_evaluation");
    }
    l["partition_policy"]["version"] = json!("catalogue-leakage@1");
    for group in l["groups"].as_array_mut().unwrap() {
        let mut related = BTreeSet::new();
        for case_id in group["case_ids"].as_array().unwrap() {
            let c = i["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| &c["case_id"] == case_id)
                .unwrap();
            let label = b["labels"]
                .as_array()
                .unwrap()
                .iter()
                .find(|label| &label["case_id"] == case_id)
                .unwrap();
            for id in c["source_ids"]
                .as_array()
                .unwrap()
                .iter()
                .chain(label["origin"]["source_ids"].as_array().unwrap())
            {
                related.insert(id.as_str().unwrap().to_owned());
            }
            let mut labelled: BTreeSet<&str> = label["hard_negative_target_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if label["truth"]["kind"] == "acceptable_targets" {
                labelled.extend(
                    label["truth"]["target_ids"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap()),
                );
            }
            for candidate in c["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| labelled.contains(c["node_id"].as_str().unwrap()))
            {
                for id in candidate["source_ids"].as_array().unwrap() {
                    related.insert(id.as_str().unwrap().to_owned());
                }
            }
        }
        group["related_source_ids"] = json!(related.into_iter().collect::<Vec<_>>());
    }
}
