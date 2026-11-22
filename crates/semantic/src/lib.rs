//! Eval-gated semantic hop resolver (T2, SPEC-00 §2, §13; US-0008).
//!
//! Only explicit `Gap` destinations are eligible. Candidate links first enter
//! a staging [`SemanticProposal`] list, carry Semantic/InferredStrong
//! provenance with evidence from both sides, and become an ephemeral graph
//! overlay only through the host independent-family gate. Legacy pair evaluation
//! is calibration, not production admission authority. Confirmed T0/T1
//! facts are never mutation targets (R-INT-1).

pub mod context;
pub mod embedding_cache;
mod eval_report;
pub use eval_report::EvalReport;
pub mod evaluation;
mod family;
pub mod knowledge_ingest;
pub mod name_unique;
pub mod retrieval;
pub use family::{FamilyKey, HopKind};

use core_graph::omission::{gap_query, is_interface_dispatch};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, ProducingMethod, Provenance, Tier, may_overwrite};
use llm::{Embedding, LlmProvider, ProviderError};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

/// Stable semantic resolver identifier stored in proposal provenance.
pub const EXTRACTOR_ID: &str = "t2.semantic-ann";

/// Why an unresolved hop is held out of ANN candidacy. An excluded hop stays
/// an honest Gap in the graph — it is never embedded and never proposed —
/// and carries this reason so the register can group it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HopExclusion {
    /// The hop's query is interface dispatch (`IFace.method`): the receiver's
    /// concrete type is unknown without a type hierarchy, so no embedding
    /// can resolve it.
    DynamicDispatch,
    /// Two or more candidates of the hop's family carry its query as their
    /// exact name, ignoring ASCII case (AC-1513). Embedding a name scores
    /// every homonym alike (near 1.0), so similarity cannot choose between
    /// them; the hop needs import or receiver-type resolution instead.
    NameCollision,
}

impl HopExclusion {
    /// Stable human-readable tag, e.g. for register grouping.
    pub fn reason(&self) -> &'static str {
        match self {
            HopExclusion::DynamicDispatch => core_graph::omission::DYNAMIC_DISPATCH,
            HopExclusion::NameCollision => core_graph::omission::NAME_COLLISION,
        }
    }
}

/// A graph Gap plus the incoming edge slot that a higher tier may fill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnresolvedHop {
    /// Explicit Gap node id.
    pub gap_id: String,
    /// Existing edge source.
    pub source_id: String,
    /// Existing edge label (`PUBLISHES`, `SUBSCRIBES`, or `CALLS`).
    pub edge_label: String,
    /// Candidate family.
    pub kind: HopKind,
    /// Optional subtype, such as `sqs-queue`, used as a deterministic filter.
    pub subtype: Option<String>,
    /// Text embedded for similarity search.
    pub query: String,
    /// T0/T1 evidence that created the unresolved slot.
    pub evidence: Vec<EvidenceRef>,
    /// Exclusion from ANN candidacy, with the reason. `None` marks ordinary
    /// T2 work; `Some` marks a hop that is never embedded or proposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion: Option<HopExclusion>,
}

impl UnresolvedHop {
    /// Candidate group this hop is matched within.
    pub fn family(&self) -> FamilyKey {
        FamilyKey::new(self.kind, self.subtype.clone())
    }

    /// Whether this hop enters ANN candidacy. Excluded hops stay honest
    /// Gaps: they are never embedded and never proposed.
    pub fn is_ann_candidate(&self) -> bool {
        self.exclusion.is_none()
    }
}

/// Searchable graph target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// Existing graph node id, or stable id for a staged resource-backed
    /// Channel that is materialized only in an approved overlay.
    pub node_id: String,
    /// Candidate family.
    pub kind: HopKind,
    /// Optional deterministic subtype.
    pub subtype: Option<String>,
    /// Text embedded into the ANN index.
    pub text: String,
    /// Evidence already attached to the target node.
    pub evidence: Vec<EvidenceRef>,
    /// A target node that does not yet exist in the confirmed graph. This is
    /// used for IaC Resource candidates and never enters the stored graph.
    pub materialized_node: Option<Node>,
}

impl Candidate {
    /// Candidate group this target belongs to.
    pub fn family(&self) -> FamilyKey {
        FamilyKey::new(self.kind, self.subtype.clone())
    }
}

/// Staged T2 link. It is not a graph fact until an eval gate approves it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticProposal {
    /// Originating candidate group, assigned by the proposal builder.
    family: FamilyKey,
    /// Gap this proposal would fill.
    pub gap_id: String,
    /// Proposed edge source.
    pub source_id: String,
    /// Proposed edge destination.
    pub target_id: String,
    /// Proposed edge label.
    pub edge_label: String,
    /// Cosine similarity in `[0, 1]`.
    pub similarity: f32,
    /// Semantic/InferredStrong provenance with cited evidence.
    pub provenance: Provenance,
    /// Optional inferred target node added only to the ephemeral overlay.
    pub target_node: Option<Node>,
}

impl SemanticProposal {
    /// Candidate group this proposal came from.
    pub fn family(&self) -> &FamilyKey {
        &self.family
    }

    fn as_edge(&self, eval: &EvalReport) -> Edge {
        Edge {
            src: self.source_id.clone(),
            dst: self.target_id.clone(),
            label: self.edge_label.clone(),
            props: serde_json::json!({
                "resolver": EXTRACTOR_ID,
                "fills_gap": self.gap_id,
                "similarity": self.similarity,
                "eval": {
                    "precision": eval.precision,
                    "recall": eval.recall,
                    "precision_floor": eval.precision_floor,
                    "similarity_threshold": eval.similarity_threshold,
                },
                "prov": self.provenance,
            }),
        }
    }
}

/// One labeled semantic pair used for legacy calibration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabeledPair {
    /// Unresolved-side text.
    pub query: String,
    /// Candidate-side text.
    pub candidate: String,
    /// Ground-truth label.
    pub is_match: bool,
}

/// Result of applying approved proposals as an in-memory best-effort overlay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    /// Nodes with approved Gap nodes removed.
    pub nodes: Vec<Node>,
    /// Edges with approved Gap edges replaced by inferred edges.
    pub edges: Vec<Edge>,
    /// Number of distinct gaps filled.
    pub gaps_filled: usize,
}

/// Semantic resolver failures.
#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    /// Embedding provider failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// ANN index rejected its configuration or vectors.
    #[error("ANN index: {0}")]
    Index(String),
    /// Inputs cannot support a meaningful paired eval.
    #[error("invalid eval set: {0}")]
    InvalidEval(String),
    /// Semantic provenance could not be constructed.
    #[error("provenance: {0}")]
    Provenance(#[from] core_prov::IntegrityError),
}

/// Thin verified wrapper around the USearch Rust binding.
pub struct AnnIndex {
    index: Index,
    ids: Vec<String>,
}

impl AnnIndex {
    /// Build a cosine/F32 index. IDs and embeddings must be aligned. The
    /// embeddings are consumed: each vector is freed as soon as the index holds
    /// its copy, so a plan never holds the returned set and the index whole at
    /// once (AC-1507).
    pub fn build(ids: Vec<String>, embeddings: Vec<Embedding>) -> Result<Self, SemanticError> {
        if ids.is_empty() || embeddings.is_empty() || ids.len() != embeddings.len() {
            return Err(SemanticError::Index(
                "ids and embeddings must be non-empty and aligned".into(),
            ));
        }
        let dimensions = embeddings[0].len();
        if dimensions == 0
            || embeddings.iter().any(|embedding| {
                embedding.len() != dimensions || embedding.iter().any(|value| !value.is_finite())
            })
        {
            return Err(SemanticError::Index(
                "vectors must be finite and uniformly sized".into(),
            ));
        }
        let options = IndexOptions {
            dimensions,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            ..IndexOptions::default()
        };
        let index =
            Index::new(&options).map_err(|error| SemanticError::Index(error.to_string()))?;
        index
            .reserve(ids.len())
            .map_err(|error| SemanticError::Index(error.to_string()))?;
        for (key, embedding) in embeddings.into_iter().enumerate() {
            index
                .add(key as u64, &embedding)
                .map_err(|error| SemanticError::Index(error.to_string()))?;
        }
        Ok(Self { index, ids })
    }

    /// Return nearest IDs with cosine similarity, best first.
    pub fn search(
        &self,
        query: &Embedding,
        count: usize,
    ) -> Result<Vec<(String, f32)>, SemanticError> {
        if query.len() != self.index.dimensions() {
            return Err(SemanticError::Index(format!(
                "query dimension {} does not match index dimension {}",
                query.len(),
                self.index.dimensions()
            )));
        }
        let matches = self
            .index
            .search(query, count.min(self.ids.len()))
            .map_err(|error| SemanticError::Index(error.to_string()))?;
        let mut results: Vec<_> = matches
            .keys
            .iter()
            .zip(matches.distances.iter())
            .filter_map(|(key, distance)| {
                self.ids
                    .get(*key as usize)
                    .map(|id| (id.clone(), (1.0_f32 - *distance).clamp(0.0, 1.0)))
            })
            .collect();
        results.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        Ok(results)
    }
}

/// Recover channel/call Gap slots and searchable existing targets. A hop
/// that a live T0.5 exact-name fill covers is not T2 work and is left out
/// (#257, AC-0270); the Gap itself stays open in the graph. A hop whose
/// query is interface dispatch (`IFace.method`) is classified out of ANN
/// candidacy (#259, AC-0285): it stays in the list as an honest Gap tagged
/// with its [`HopExclusion`] reason, but is never embedded or proposed. A
/// hop whose query names two or more candidates of its family exactly is
/// held out the same way as a name collision (AC-1513).
pub fn graph_inputs(nodes: &[Node], edges: &[Edge]) -> (Vec<UnresolvedHop>, Vec<Candidate>) {
    let gaps: BTreeMap<_, _> = nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .map(|node| (node.id.as_str(), node))
        .collect();
    let filled = name_unique::live_fills(nodes, edges);
    let mut hops = Vec::new();
    for edge in edges {
        let Some(gap) = gaps.get(edge.dst.as_str()) else {
            continue;
        };
        if filled.contains(&(gap.id.clone(), edge.src.clone())) {
            continue;
        }
        let kind = match edge.label.as_str() {
            "PUBLISHES" | "SUBSCRIBES" => HopKind::Channel,
            "CALLS" => HopKind::Call,
            _ => continue,
        };
        let query = gap_query(&gap.props);
        let evidence = evidence_from(&gap.props);
        if query.is_empty() || evidence.is_empty() {
            continue;
        }
        // Interface dispatch (`IFace.method`) is classified out of ANN
        // candidacy (#259, AC-0285): the hop stays an honest Gap, tagged
        // with the exclusion reason, but is never embedded or proposed.
        let exclusion = is_interface_dispatch(&query).then_some(HopExclusion::DynamicDispatch);
        hops.push(UnresolvedHop {
            gap_id: gap.id.clone(),
            source_id: edge.src.clone(),
            edge_label: edge.label.clone(),
            kind,
            subtype: (kind == HopKind::Channel)
                .then(|| gap.props["kind"].as_str().map(String::from))
                .flatten(),
            query,
            evidence,
            exclusion,
        });
    }
    hops.sort_by(|a, b| a.gap_id.cmp(&b.gap_id));

    let mut candidates = Vec::new();
    for node in nodes {
        let (kind, subtype, text, materialized_node) = match node.label.as_str() {
            "Channel" => (
                HopKind::Channel,
                node.props["kind"].as_str().map(String::from),
                node.props["identity"].as_str().map(String::from),
                None,
            ),
            "Symbol" | "Component" => (
                HopKind::Call,
                None,
                node.props["name"]
                    .as_str()
                    .or_else(|| node.id.split('#').next_back())
                    .map(String::from),
                None,
            ),
            "Resource" => {
                let Some(channel_kind) =
                    node.props["type"].as_str().and_then(resource_channel_kind)
                else {
                    continue;
                };
                let Some(logical_id) = node.props["logical_id"].as_str() else {
                    continue;
                };
                let target_id = format!("chan:{channel_kind}:resource:{}", node.id);
                (
                    HopKind::Channel,
                    Some(channel_kind.to_string()),
                    Some(format!(
                        "{} {}",
                        logical_id.replace(['.', '_', '-'], " "),
                        node.props["type"].as_str().unwrap_or_default()
                    )),
                    Some(Node {
                        id: target_id,
                        label: "Channel".into(),
                        props: serde_json::json!({
                            "kind": channel_kind,
                            "identity": logical_id,
                            "backing_resource": node.id,
                            "staged": true,
                        }),
                    }),
                )
            }
            _ => continue,
        };
        let evidence = evidence_from(&node.props);
        if let Some(text) = text
            && !text.trim().is_empty()
            && !evidence.is_empty()
        {
            candidates.push(Candidate {
                node_id: materialized_node
                    .as_ref()
                    .map(|target| target.id.clone())
                    .unwrap_or_else(|| node.id.clone()),
                kind,
                subtype,
                text,
                evidence,
                materialized_node,
            });
        }
    }
    candidates.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    exclude_name_collisions(&mut hops, &candidates);
    (hops, candidates)
}

/// Text key a hop query and a candidate name share when they are the same
/// name: ASCII case is folded because the embedding model scores `basename`
/// and `baseName` alike.
fn name_key(text: &str) -> String {
    text.trim().to_ascii_lowercase()
}

/// Mark every still-eligible hop whose query equals the name of two or more
/// candidates in its family as a [`HopExclusion::NameCollision`] (AC-1513).
/// A single same-name candidate stays eligible: it is the T0.5 rule's
/// territory, and similarity does point at it.
pub fn exclude_name_collisions(hops: &mut [UnresolvedHop], candidates: &[Candidate]) {
    let mut names: BTreeMap<(FamilyKey, String), usize> = BTreeMap::new();
    for candidate in candidates {
        *names
            .entry((candidate.family(), name_key(&candidate.text)))
            .or_default() += 1;
    }
    for hop in hops.iter_mut().filter(|hop| hop.exclusion.is_none()) {
        if names
            .get(&(hop.family(), name_key(&hop.query)))
            .is_some_and(|count| *count >= 2)
        {
            hop.exclusion = Some(HopExclusion::NameCollision);
        }
    }
}

fn resource_channel_kind(resource_type: &str) -> Option<&'static str> {
    match resource_type {
        "aws_sqs_queue" => Some("sqs-queue"),
        "aws_sns_topic" => Some("sns-topic"),
        _ => None,
    }
}

fn evidence_from(props: &serde_json::Value) -> Vec<EvidenceRef> {
    serde_json::from_value(props["prov"]["evidence"].clone()).unwrap_or_default()
}

/// Produce staged ANN proposals. No graph mutation occurs here.
fn candidate_groups(candidates: &[Candidate]) -> BTreeMap<FamilyKey, Vec<&Candidate>> {
    let mut grouped: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for candidate in candidates {
        grouped
            .entry(candidate.family())
            .or_default()
            .push(candidate);
    }
    grouped
}

/// Exact text plan shared with the proposal grouping; labels never enter cache keys.
/// No candidates from groups without matching unresolved hops are embedded.
pub fn embedding_inputs(
    hops: &[UnresolvedHop],
    candidates: &[Candidate],
    pairs: &[LabeledPair],
) -> Result<Vec<String>, SemanticError> {
    if pairs.len() > embedding_cache::MAX_ROWS / 2
        || candidates.len() > embedding_cache::MAX_ROWS
        || hops.len() > embedding_cache::MAX_ROWS
    {
        return Err(SemanticError::Index(format!(
            "embedding plan count bounds: {} pairs (max {}), {} candidates (max {}), {} hops (max {})",
            pairs.len(),
            embedding_cache::MAX_ROWS / 2,
            candidates.len(),
            embedding_cache::MAX_ROWS,
            hops.len(),
            embedding_cache::MAX_ROWS,
        )));
    }
    let mut texts: Vec<&String> = pairs
        .iter()
        .flat_map(|pair| [&pair.query, &pair.candidate])
        .collect();
    for (key, compatible) in candidate_groups(candidates) {
        let matching: Vec<_> = hops
            .iter()
            .filter(|hop| hop.is_ann_candidate() && hop.family() == key)
            .collect();
        if matching.is_empty() {
            continue;
        }
        texts.extend(compatible.into_iter().map(|candidate| &candidate.text));
        texts.extend(matching.into_iter().map(|hop| &hop.query));
    }
    let longest = texts.iter().map(|t| t.len()).max().unwrap_or(0);
    let total = texts.iter().try_fold(0usize, |n, t| n.checked_add(t.len()));
    if texts.len() > embedding_cache::MAX_ROWS
        || longest > embedding_cache::MAX_TEXT_BYTES
        || total.is_none_or(|n| n > embedding_cache::MAX_INPUT_BYTES)
    {
        // Counts only, never text: the caller can tell which bound tripped.
        return Err(SemanticError::Index(format!(
            "embedding plan bounds: {} texts (max {}), longest {} bytes (max {}), {} bytes (max {})",
            texts.len(),
            embedding_cache::MAX_ROWS,
            longest,
            embedding_cache::MAX_TEXT_BYTES,
            total.unwrap_or(usize::MAX),
            embedding_cache::MAX_INPUT_BYTES,
        )));
    }
    Ok(texts.into_iter().cloned().collect())
}

/// Embeds candidates and ANN-eligible hops per family and stages a proposal
/// for each hop's `neighbors` nearest candidates. Proposals are T2, not graph
/// facts, until an eval gate approves them.
pub fn propose(
    provider: &dyn LlmProvider,
    hops: &[UnresolvedHop],
    candidates: &[Candidate],
    neighbors: usize,
) -> Result<Vec<SemanticProposal>, SemanticError> {
    let grouped = candidate_groups(candidates);
    let mut proposals = Vec::new();
    for (key, compatible) in grouped {
        let compatible_hops: Vec<&UnresolvedHop> = hops
            .iter()
            .filter(|hop| hop.is_ann_candidate())
            .filter(|hop| hop.family() == key)
            .collect();
        if compatible_hops.is_empty() {
            continue;
        }
        let texts: Vec<String> = compatible.iter().map(|item| item.text.clone()).collect();
        let embeddings = provider.embed(&texts)?;
        let ids: Vec<String> = compatible.iter().map(|item| item.node_id.clone()).collect();
        let index = AnnIndex::build(ids, embeddings)?;
        let query_texts: Vec<String> = compatible_hops
            .iter()
            .map(|hop| hop.query.clone())
            .collect();
        let query_embeddings = provider.embed(&query_texts)?;
        if query_embeddings.len() != compatible_hops.len() {
            return Err(SemanticError::Index(format!(
                "provider returned {} embeddings for {} unresolved hops",
                query_embeddings.len(),
                compatible_hops.len()
            )));
        }
        let by_id: BTreeMap<_, _> = compatible
            .iter()
            .map(|candidate| (candidate.node_id.as_str(), *candidate))
            .collect();
        for (hop, query) in compatible_hops.into_iter().zip(&query_embeddings) {
            for (target_id, similarity) in index.search(query, neighbors.max(1))? {
                let Some(candidate) = by_id.get(target_id.as_str()) else {
                    continue;
                };
                let mut evidence = hop.evidence.clone();
                evidence.extend(candidate.evidence.clone());
                evidence.sort_by(|a, b| {
                    (&a.repo, &a.path, a.byte_start, a.byte_end, &a.commit_sha).cmp(&(
                        &b.repo,
                        &b.path,
                        b.byte_start,
                        b.byte_end,
                        &b.commit_sha,
                    ))
                });
                evidence.dedup();
                let fact = format!(
                    "{}:{}:{}:{}:{similarity:.6}",
                    hop.gap_id, hop.source_id, hop.edge_label, target_id
                );
                let method = ProducingMethod::Model {
                    model: provider.embedding_model_id(),
                };
                let provenance = Provenance::new(
                    Tier::Semantic,
                    ConfidenceTier::InferredStrong,
                    evidence.clone(),
                    EXTRACTOR_ID,
                    fact.as_bytes(),
                )?
                .with_method(method.clone());
                let target_node = candidate
                    .materialized_node
                    .as_ref()
                    .map(|template| {
                        let mut node = template.clone();
                        let node_provenance = Provenance::new(
                            Tier::Semantic,
                            ConfidenceTier::InferredStrong,
                            evidence,
                            EXTRACTOR_ID,
                            format!("materialize {}", node.id).as_bytes(),
                        )?
                        .with_method(method.clone());
                        node.props["prov"] = serde_json::to_value(node_provenance)
                            .expect("semantic provenance serializes");
                        Ok::<Node, SemanticError>(node)
                    })
                    .transpose()?;
                proposals.push(SemanticProposal {
                    family: key.clone(),
                    gap_id: hop.gap_id.clone(),
                    source_id: hop.source_id.clone(),
                    target_id,
                    edge_label: hop.edge_label.clone(),
                    similarity,
                    provenance,
                    target_node,
                });
            }
        }
    }
    proposals.sort_by(|a, b| {
        a.gap_id
            .cmp(&b.gap_id)
            .then_with(|| {
                b.similarity
                    .partial_cmp(&a.similarity)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| a.target_id.cmp(&b.target_id))
    });
    Ok(proposals)
}

/// Measure labeled pairs and choose the highest-recall operating point that
/// clears `precision_floor` (ties prefer higher precision, then threshold).
pub fn evaluate(
    provider: &dyn LlmProvider,
    pairs: &[LabeledPair],
    precision_floor: f32,
) -> Result<EvalReport, SemanticError> {
    if !(0.0..=1.0).contains(&precision_floor) {
        return Err(SemanticError::InvalidEval(
            "precision floor must be between 0 and 1".into(),
        ));
    }
    let positives = pairs.iter().filter(|pair| pair.is_match).count();
    let negatives = pairs.len().saturating_sub(positives);
    if positives == 0 || negatives == 0 {
        return Err(SemanticError::InvalidEval(
            "paired eval requires positive and negative labels".into(),
        ));
    }
    let texts: Vec<String> = pairs
        .iter()
        .flat_map(|pair| [pair.query.clone(), pair.candidate.clone()])
        .collect();
    let embeddings = provider.embed(&texts)?;
    if embeddings.len() != texts.len() {
        return Err(SemanticError::InvalidEval(format!(
            "provider returned {} embeddings for {} eval texts",
            embeddings.len(),
            texts.len()
        )));
    }
    let mut scored = Vec::with_capacity(pairs.len());
    for (index, pair) in pairs.iter().enumerate() {
        let left = &embeddings[index * 2];
        let right = &embeddings[index * 2 + 1];
        scored.push((cosine(left, right)?, pair.is_match));
    }
    let mut thresholds: Vec<f32> = scored.iter().map(|(score, _)| *score).collect();
    thresholds.sort_by(|a, b| b.partial_cmp(a).unwrap_or(Ordering::Equal));
    thresholds.dedup_by(|a, b| (*a - *b).abs() < f32::EPSILON);

    let mut best: Option<EvalReport> = None;
    for threshold in thresholds {
        let (mut tp, mut fp, mut fn_) = (0, 0, 0);
        for (score, expected) in &scored {
            let predicted = *score >= threshold;
            match (predicted, *expected) {
                (true, true) => tp += 1,
                (true, false) => fp += 1,
                (false, true) => fn_ += 1,
                (false, false) => {}
            }
        }
        let precision = tp as f32 / (tp + fp).max(1) as f32;
        let recall = tp as f32 / (tp + fn_).max(1) as f32;
        let report = EvalReport {
            precision_floor,
            similarity_threshold: threshold,
            precision,
            recall,
            passed: tp > 0 && precision >= precision_floor,
            true_positives: tp,
            false_positives: fp,
            false_negatives: fn_,
        };
        if !report.passed {
            continue;
        }
        let replace = best.as_ref().is_none_or(|current| {
            report.recall > current.recall
                || (report.recall == current.recall && report.precision > current.precision)
                || (report.recall == current.recall
                    && report.precision == current.precision
                    && report.similarity_threshold > current.similarity_threshold)
        });
        if replace {
            best = Some(report);
        }
    }
    Ok(best.unwrap_or(EvalReport {
        precision_floor,
        similarity_threshold: 1.0,
        precision: 0.0,
        recall: 0.0,
        passed: false,
        true_positives: 0,
        false_positives: 0,
        false_negatives: positives,
    }))
}

fn cosine(left: &[f32], right: &[f32]) -> Result<f32, SemanticError> {
    if left.is_empty() || left.len() != right.len() {
        return Err(SemanticError::InvalidEval(
            "embedding dimensions must be non-empty and equal".into(),
        ));
    }
    let dot: f32 = left.iter().zip(right).map(|(a, b)| a * b).sum();
    let left_norm: f32 = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm: f32 = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        return Err(SemanticError::InvalidEval(
            "zero-length embedding norm".into(),
        ));
    }
    Ok((dot / (left_norm * right_norm)).clamp(0.0, 1.0))
}

/// Admit only proposals at the calibrated operating point after a passing
/// aggregate precision gate. The best candidate per Gap wins deterministically.
pub fn gated_proposals(proposals: &[SemanticProposal], eval: &EvalReport) -> Vec<SemanticProposal> {
    if !eval.passed || eval.precision < eval.precision_floor {
        return Vec::new();
    }
    let mut ordered = proposals.to_vec();
    ordered.sort_by(|a, b| {
        a.gap_id
            .cmp(&b.gap_id)
            .then_with(|| {
                b.similarity
                    .partial_cmp(&a.similarity)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| a.target_id.cmp(&b.target_id))
    });
    let mut seen = BTreeSet::new();
    ordered
        .into_iter()
        // Calibration uses exact cosine math while ANN proposal scores come
        // from USearch. Permit only the sub-micro rounding difference between
        // those backends; materially sub-threshold links still fail closed.
        .filter(|proposal| proposal.similarity + 1.0e-6 >= eval.similarity_threshold)
        .filter(|proposal| seen.insert(proposal.gap_id.clone()))
        .collect()
}

/// Build a best-effort overlay without mutating the confirmed graph. Only an
/// existing Gap edge is replaced; all other facts are copied unchanged.
pub fn overlay(
    nodes: &[Node],
    edges: &[Edge],
    approved: &[SemanticProposal],
    eval: &EvalReport,
) -> Overlay {
    overlay_with(nodes, edges, approved, |proposal| proposal.as_edge(eval))
}

pub(crate) fn overlay_with(
    nodes: &[Node],
    edges: &[Edge],
    approved: &[SemanticProposal],
    edge: impl Fn(&SemanticProposal) -> Edge,
) -> Overlay {
    let gap_ids: HashSet<&str> = nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .map(|node| node.id.as_str())
        .collect();
    debug_assert!(may_overwrite(Tier::Semantic, ConfidenceTier::Gap));
    let approved: BTreeMap<&str, &SemanticProposal> = approved
        .iter()
        .filter(|proposal| gap_ids.contains(proposal.gap_id.as_str()))
        .filter(|proposal| {
            edges.iter().any(|edge| {
                edge.src == proposal.source_id
                    && edge.dst == proposal.gap_id
                    && edge.label == proposal.edge_label
                    && edge.props["prov"]["confidence_tier"] == "Gap"
            })
        })
        .map(|proposal| (proposal.gap_id.as_str(), proposal))
        .collect();
    let resolved: HashSet<&str> = approved.keys().copied().collect();
    let mut overlay_nodes: Vec<Node> = nodes
        .iter()
        .filter(|node| !resolved.contains(node.id.as_str()))
        .cloned()
        .collect();
    let existing_ids: HashSet<String> = overlay_nodes.iter().map(|node| node.id.clone()).collect();
    overlay_nodes.extend(
        approved
            .values()
            .filter_map(|proposal| proposal.target_node.clone())
            .filter(|node| !existing_ids.contains(&node.id)),
    );
    overlay_nodes.sort_by(|a, b| a.id.cmp(&b.id));
    overlay_nodes.dedup_by(|a, b| a.id == b.id);
    let mut overlay_edges: Vec<Edge> = edges
        .iter()
        .filter(|edge| !resolved.contains(edge.dst.as_str()))
        .cloned()
        .collect();
    overlay_edges.extend(approved.values().map(|proposal| edge(proposal)));
    overlay_edges.sort_by(|a, b| (&a.src, &a.dst, &a.label).cmp(&(&b.src, &b.dst, &b.label)));
    Overlay {
        nodes: overlay_nodes,
        edges: overlay_edges,
        gaps_filled: approved.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graph::omission::{Omission, OmissionClass};
    use llm::{Locality, ProviderCaps};
    use std::time::{Duration, Instant};

    struct KeywordProvider;

    impl LlmProvider for KeywordProvider {
        fn id(&self) -> &str {
            "test-keywords"
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

        fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Ok(batch
                .iter()
                .map(|text| {
                    let text = text.to_ascii_lowercase();
                    vec![
                        f32::from(text.contains("order")),
                        f32::from(text.contains("user")),
                        f32::from(text.contains("billing")),
                        0.01,
                    ]
                })
                .collect())
        }
    }

    fn evidence(path: &str) -> serde_json::Value {
        serde_json::json!({
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "evidence": [{
                "repo": "local/shop",
                "path": path,
                "byte_start": 1,
                "byte_end": 5,
                "commit_sha": "abc123"
            }],
            "extractor_id": "t0.test",
            "content_hash": "hash"
        })
    }

    fn graph() -> (Vec<Node>, Vec<Edge>) {
        let nodes = vec![
            Node {
                id: "sym:shop@send.ts#send".into(),
                label: "Symbol".into(),
                props: serde_json::json!({"name": "send", "prov": evidence("send.ts")}),
            },
            Node {
                id: "gap:chan:shop@send.ts@1".into(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue",
                    "raw": "computed order destination",
                    "reason": "runtime-computed channel identity",
                    "prov": evidence("send.ts")
                }),
            },
            Node {
                id: "chan:sqs-queue:orders".into(),
                label: "Channel".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue", "identity": "orders queue", "prov": evidence("infra.tf")
                }),
            },
            Node {
                id: "chan:sqs-queue:users".into(),
                label: "Channel".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue", "identity": "users queue", "prov": evidence("infra.tf")
                }),
            },
        ];
        let edges = vec![Edge {
            src: "sym:shop@send.ts#send".into(),
            dst: "gap:chan:shop@send.ts@1".into(),
            label: "PUBLISHES".into(),
            props: serde_json::json!({"prov": {
                "confidence_tier": "Gap", "evidence": evidence("send.ts")["evidence"]
            }}),
        }];
        (nodes, edges)
    }

    fn eval_pairs() -> Vec<LabeledPair> {
        vec![
            LabeledPair {
                query: "order destination".into(),
                candidate: "orders queue".into(),
                is_match: true,
            },
            LabeledPair {
                query: "order destination".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
            LabeledPair {
                query: "billing event".into(),
                candidate: "billing channel".into(),
                is_match: true,
            },
            LabeledPair {
                query: "billing event".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
        ]
    }

    #[test]
    fn usearch_returns_cosine_nearest_neighbor() {
        let index = AnnIndex::build(
            vec!["orders".into(), "users".into()],
            vec![vec![1.0, 0.0], vec![0.0, 1.0]],
        )
        .unwrap();
        let hits = index.search(&vec![0.99, 0.01], 2).unwrap();
        assert_eq!(hits[0].0, "orders");
        assert!(hits[0].1 > hits[1].1);
    }

    #[test]
    fn semantic_proposals_are_inferred_strong_and_cite_both_sides() {
        // AC-0021: unresolved hops become staged T2 proposals with evidence.
        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        assert_eq!(proposals[0].target_id, "chan:sqs-queue:orders");
        assert_eq!(proposals[0].provenance.tier, Tier::Semantic);
        assert_eq!(
            proposals[0].provenance.confidence_tier,
            ConfidenceTier::InferredStrong
        );
        assert_eq!(proposals[0].provenance.evidence.len(), 2);
    }

    #[test]
    fn infra_resources_stage_channel_nodes_for_computed_gaps() {
        // AC-0021: real T0 ingestion may have only a computed channel Gap and
        // an IaC Resource. The resource is a semantic candidate, while the
        // inferred Channel exists only in the approved best-effort overlay.
        let (mut nodes, edges) = graph();
        nodes.retain(|node| node.label != "Channel");
        nodes.extend([
            Node {
                id: "res:shop@aws_sqs_queue.orders".into(),
                label: "Resource".into(),
                props: serde_json::json!({
                    "type": "aws_sqs_queue",
                    "logical_id": "aws_sqs_queue.orders",
                    "prov": evidence("infra.tf")
                }),
            },
            Node {
                id: "res:shop@aws_sqs_queue.users".into(),
                label: "Resource".into(),
                props: serde_json::json!({
                    "type": "aws_sqs_queue",
                    "logical_id": "aws_sqs_queue.users",
                    "prov": evidence("infra.tf")
                }),
            },
        ]);
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let channel_candidates: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.kind == HopKind::Channel)
            .collect();
        assert_eq!(channel_candidates.len(), 2);
        assert!(
            channel_candidates
                .iter()
                .all(|candidate| candidate.materialized_node.is_some())
        );
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        let report = evaluate(&KeywordProvider, &eval_pairs(), 0.95).unwrap();
        let approved = gated_proposals(&proposals, &report);
        let preview = overlay(&nodes, &edges, &approved, &report);
        assert_eq!(preview.gaps_filled, 1);
        let channel = preview
            .nodes
            .iter()
            .find(|node| node.label == "Channel")
            .expect("approved resource target materializes an inferred channel");
        assert_eq!(
            channel.props["backing_resource"],
            "res:shop@aws_sqs_queue.orders"
        );
        assert_eq!(channel.props["prov"]["tier"], "Semantic");
        assert_eq!(channel.props["prov"]["confidence_tier"], "InferredStrong");
        assert!(nodes.iter().all(|node| node.label != "Channel"));
    }

    // AC-0271: family attribution comes from the exact matching group, not
    // shared embedding text, and absent channel subtype never acts as wildcard.
    #[test]
    fn proposal_families_preserve_exact_hop_and_candidate_origin() {
        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let keys = [
            FamilyKey::new(HopKind::Call, None),
            FamilyKey::new(HopKind::Channel, None),
            FamilyKey::new(HopKind::Channel, Some("sqs-queue".into())),
            FamilyKey::new(HopKind::Channel, Some("sns-topic".into())),
        ];
        let exact_call = FamilyKey::new(HopKind::Call, Some("unchanged".into()));
        assert_ne!(exact_call, keys[0]);
        let mut encoded = serde_json::to_value(&exact_call).unwrap();
        assert_eq!(
            serde_json::from_value::<FamilyKey>(encoded.clone()).unwrap(),
            exact_call
        );
        encoded["authority"] = serde_json::json!(true);
        assert!(serde_json::from_value::<FamilyKey>(encoded).is_err());
        let mut grouped_hops = Vec::new();
        let mut grouped_candidates = Vec::new();
        for (index, family) in keys.iter().enumerate() {
            let mut hop = hops[0].clone();
            hop.gap_id = format!("gap:{index}");
            hop.kind = family.kind();
            hop.subtype = family.subtype().map(str::to_owned);
            hop.query = "identical order text".into();
            let mut candidate = candidates[0].clone();
            candidate.node_id = format!("target:{index}");
            candidate.kind = family.kind();
            candidate.subtype = family.subtype().map(str::to_owned);
            candidate.text = hop.query.clone();
            assert_eq!(hop.family(), *family);
            assert_eq!(candidate.family(), *family);
            grouped_hops.push(hop);
            grouped_candidates.push(candidate);
        }
        let proposals = propose(&KeywordProvider, &grouped_hops, &grouped_candidates, 4).unwrap();
        assert_eq!(proposals.len(), keys.len());
        for (index, proposal) in proposals.iter().enumerate() {
            assert_eq!(proposal.gap_id, format!("gap:{index}"));
            assert_eq!(proposal.target_id, format!("target:{index}"));
            assert_eq!(proposal.family(), &keys[index]);
        }
        // A second execution preserves family origin and deterministic order.
        assert_eq!(
            proposals,
            propose(&KeywordProvider, &grouped_hops, &grouped_candidates, 4).unwrap()
        );
    }

    #[test]
    fn semantic_resolver_handles_explicit_call_gaps() {
        let nodes = vec![
            Node {
                id: "sym:shop@caller.ts#run".into(),
                label: "Symbol".into(),
                props: serde_json::json!({"name": "run", "prov": evidence("caller.ts")}),
            },
            Node {
                id: "gap:call:shop@caller.ts@8".into(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "callee": "process order",
                    "reason": "unresolved call target",
                    "prov": evidence("caller.ts")
                }),
            },
            Node {
                id: "sym:shop@orders.ts#processOrder".into(),
                label: "Symbol".into(),
                props: serde_json::json!({
                    "name": "process order", "prov": evidence("orders.ts")
                }),
            },
            Node {
                id: "sym:shop@users.ts#processUser".into(),
                label: "Symbol".into(),
                props: serde_json::json!({
                    "name": "process user", "prov": evidence("users.ts")
                }),
            },
        ];
        let edges = vec![Edge {
            src: "sym:shop@caller.ts#run".into(),
            dst: "gap:call:shop@caller.ts@8".into(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov": {
                "confidence_tier": "Gap", "evidence": evidence("caller.ts")["evidence"]
            }}),
        }];
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        assert_eq!(hops[0].kind, HopKind::Call);
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        assert_eq!(proposals[0].target_id, "sym:shop@orders.ts#processOrder");
    }

    #[test]
    fn paired_eval_gates_best_effort_overlay() {
        // AC-0022: only a calibrated, passing precision floor can replace a
        // Gap in the best-effort overlay; confirmed graph input is untouched.
        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        let report = evaluate(&KeywordProvider, &eval_pairs(), 0.95).unwrap();
        assert!(report.passed);
        assert_eq!(report.precision, 1.0);
        let approved = gated_proposals(&proposals, &report);
        assert_eq!(approved.len(), 1);
        let preview = overlay(&nodes, &edges, &approved, &report);
        assert_eq!(preview.gaps_filled, 1);
        assert!(preview.nodes.iter().all(|node| node.label != "Gap"));
        let edge = preview
            .edges
            .iter()
            .find(|edge| edge.label == "PUBLISHES")
            .unwrap();
        assert_eq!(edge.dst, "chan:sqs-queue:orders");
        assert_eq!(edge.props["prov"]["tier"], "Semantic");
        assert_eq!(edge.props["prov"]["confidence_tier"], "InferredStrong");
        assert!(nodes.iter().any(|node| node.label == "Gap"));
    }

    #[test]
    fn gating_tolerates_only_backend_rounding_at_the_calibrated_threshold() {
        // AC-0022: exact evaluator cosine and ANN cosine can differ by one
        // floating-point rounding step without changing the calibrated gate.
        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let mut proposal = propose(&KeywordProvider, &hops, &candidates, 1)
            .unwrap()
            .remove(0);
        let mut report = evaluate(&KeywordProvider, &eval_pairs(), 0.95).unwrap();
        report.similarity_threshold = 1.0;

        proposal.similarity = 0.999_999_94;
        assert_eq!(gated_proposals(&[proposal.clone()], &report).len(), 1);

        proposal.similarity = 0.999;
        assert!(gated_proposals(&[proposal], &report).is_empty());
    }

    #[test]
    fn failed_precision_gate_exports_no_semantic_links() {
        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        let misleading = vec![
            LabeledPair {
                query: "order".into(),
                candidate: "orders".into(),
                is_match: false,
            },
            LabeledPair {
                query: "billing".into(),
                candidate: "billing".into(),
                is_match: true,
            },
        ];
        let report = evaluate(&KeywordProvider, &misleading, 1.0).unwrap();
        // A perfect threshold still exists for this tiny set only if the two
        // scores differ; here they tie, so admitting the positive admits the
        // false positive and the gate fails closed.
        assert!(!report.passed);
        assert!(gated_proposals(&proposals, &report).is_empty());
    }

    #[test]
    #[ignore = "requires local Ollama with nomic-embed-text (MT-M7-01)"]
    fn real_ollama_resolves_eval_gated_gap() {
        let provider = llm::OllamaProvider::local_default().unwrap();
        assert_eq!(provider.locality(), Locality::Local);
        let pairs = vec![
            LabeledPair {
                query: "runtime destination for an order event".into(),
                candidate: "orders queue".into(),
                is_match: true,
            },
            LabeledPair {
                query: "runtime destination for a user event".into(),
                candidate: "users queue".into(),
                is_match: true,
            },
            LabeledPair {
                query: "runtime destination for a billing event".into(),
                candidate: "billing channel".into(),
                is_match: true,
            },
            LabeledPair {
                query: "runtime destination for an order event".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
            LabeledPair {
                query: "runtime destination for a billing event".into(),
                candidate: "orders queue".into(),
                is_match: false,
            },
            LabeledPair {
                query: "runtime destination for a user event".into(),
                candidate: "billing channel".into(),
                is_match: false,
            },
        ];
        let report = evaluate(&provider, &pairs, 0.9).unwrap();
        assert!(report.passed, "paired eval: {report:?}");

        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let proposals = propose(&provider, &hops, &candidates, 2).unwrap();
        let approved = gated_proposals(&proposals, &report);
        let preview = overlay(&nodes, &edges, &approved, &report);
        assert_eq!(preview.gaps_filled, 1, "proposals: {proposals:?}");

        let texts: Vec<String> = (0..256).map(|index| format!("candidate {index}")).collect();
        let embeddings = provider.embed(&texts).unwrap();
        let ids = (0..embeddings.len())
            .map(|index| format!("candidate-{index}"))
            .collect();
        let first = embeddings[0].clone();
        let index = AnnIndex::build(ids, embeddings).unwrap();
        let started = Instant::now();
        let hits = index.search(&first, 10).unwrap();
        let elapsed = started.elapsed();
        assert!(!hits.is_empty());
        assert!(
            elapsed < Duration::from_millis(100),
            "ANN query took {elapsed:?}"
        );
        println!(
            "provider={} precision={:.3} recall={:.3} threshold={:.3} ann={elapsed:?} gaps_filled={}",
            provider.id(),
            report.precision,
            report.recall,
            report.similarity_threshold,
            preview.gaps_filled
        );
    }

    #[test]
    fn semantic_proposals_record_the_embedding_model_as_method() {
        // AC-0244 (T-0244): T2 output names the model that produced it.
        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        assert!(!proposals.is_empty());
        for proposal in &proposals {
            assert_eq!(
                proposal.provenance.method,
                ProducingMethod::Model {
                    model: "test-keywords".into()
                }
            );
            assert_eq!(proposal.provenance.tier, Tier::Semantic);
        }
    }

    // AC-0285 (T-0285): an interface-dispatch hop (`IFace.method`) is
    // classified out of ANN candidacy — never embedded, never proposed —
    // and stays an honest Gap tagged with the exclusion reason.
    #[test]
    fn interface_dispatch_hops_are_excluded_from_ann_candidacy() {
        let nodes = vec![
            Node {
                id: "sym:shop@caller.ts#run".into(),
                label: "Symbol".into(),
                props: serde_json::json!({"name": "run", "prov": evidence("caller.ts")}),
            },
            Node {
                id: "gap:call:shop@caller.ts@8".into(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "callee": "IChannel.listen",
                    "reason": "unresolved call target",
                    "prov": evidence("caller.ts")
                }),
            },
            Node {
                id: "sym:shop@channel.ts#listen".into(),
                label: "Symbol".into(),
                props: serde_json::json!({
                    "name": "IChannel.listen", "prov": evidence("channel.ts")
                }),
            },
        ];
        let edges = vec![Edge {
            src: "sym:shop@caller.ts#run".into(),
            dst: "gap:call:shop@caller.ts@8".into(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov": {
                "confidence_tier": "Gap", "evidence": evidence("caller.ts")["evidence"]
            }}),
        }];
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        // The hop stays an honest Gap, tagged with the exclusion reason.
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].exclusion, Some(HopExclusion::DynamicDispatch));
        assert_eq!(
            hops[0]
                .exclusion
                .expect("excluded hop carries a reason")
                .reason(),
            "dynamic dispatch - needs type hierarchy"
        );
        assert!(!hops[0].is_ann_candidate());
        // It never produces proposals, even against a same-text candidate.
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        assert!(proposals.is_empty());
        // Its query is never embedded: the family has no eligible hop, so
        // neither the query nor its candidates enter the embedding plan.
        let texts = embedding_inputs(&hops, &candidates, &[]).unwrap();
        assert!(texts.is_empty());
        // The Gap itself is untouched in the graph.
        assert!(nodes.iter().any(|node| node.label == "Gap"));
    }

    // AC-1513 (T-1513): a hop whose query names two or more candidates of its
    // family exactly (ASCII case folded) is held out as a name collision:
    // never embedded, never proposed, an honest Gap with its reason. One
    // same-name candidate, or none, leaves the hop eligible.
    #[test]
    fn name_collision_hops_are_excluded_from_ann_candidacy() {
        let symbol = |id: &str, name: &str| Node {
            id: format!("sym:shop@{id}"),
            label: "Symbol".into(),
            props: serde_json::json!({"name": name, "kind": "Function", "prov": evidence("lib.ts")}),
        };
        let gap = |at: u32, callee: &str| Node {
            id: format!("gap:call:shop@caller.ts@{at}"),
            label: "Gap".into(),
            props: serde_json::json!({
                "callee": callee, "reason": "unresolved call target", "prov": evidence("caller.ts")
            }),
        };
        let nodes = vec![
            symbol("caller.ts#run", "run"),
            symbol("a/filters.ts#join", "join"),
            symbol("b/filters.ts#join", "join"),
            symbol("a/paths.ts#basename", "basename"),
            symbol("b/model.ts#baseName", "baseName"),
            symbol("orders.ts#mergeOrders", "mergeOrders"),
            gap(1, "join"),
            gap(2, "BaseName"),
            gap(3, "merge orders"),
        ];
        let edges: Vec<_> = (1..=3)
            .map(|at| Edge {
                src: "sym:shop@caller.ts#run".into(),
                dst: format!("gap:call:shop@caller.ts@{at}"),
                label: "CALLS".into(),
                props: serde_json::json!({"prov": {
                    "confidence_tier": "Gap", "evidence": evidence("caller.ts")["evidence"]
                }}),
            })
            .collect();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let by_query = |q: &str| hops.iter().find(|h| h.query == q).unwrap();
        for collided in ["join", "BaseName"] {
            let hop = by_query(collided);
            assert_eq!(hop.exclusion, Some(HopExclusion::NameCollision));
            assert_eq!(
                hop.exclusion.unwrap().reason(),
                "name collision - needs import or type resolution"
            );
        }
        assert!(by_query("merge orders").is_ann_candidate());
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        assert!(!proposals.is_empty());
        assert!(proposals.iter().all(|p| p.gap_id.ends_with("@3")));
        // A collided query is never embedded (`join` is embedded only as a
        // candidate name; `BaseName` names no candidate verbatim).
        let texts = embedding_inputs(&hops, &candidates, &[]).unwrap();
        assert!(texts.iter().all(|t| t != "BaseName"));
        assert!(texts.iter().any(|t| t == "merge orders"));
        // A single same-name candidate leaves the hop eligible.
        let mut single = hops.clone();
        single.iter_mut().for_each(|h| h.exclusion = None);
        let one: Vec<_> = candidates
            .iter()
            .filter(|c| !c.node_id.contains("b/"))
            .cloned()
            .collect();
        exclude_name_collisions(&mut single, &one);
        assert!(single.iter().all(UnresolvedHop::is_ann_candidate));
    }

    // AC-1507: a whole-VSCode plan (476,907 candidate symbols and 81,654 hops,
    // measured 2026-11-22, all in one family here) is admitted whole; the next
    // bound still refuses.
    #[test]
    fn vscode_scale_embedding_plans_are_admitted() {
        let candidate = |n: usize| Candidate {
            node_id: format!("sym:vscode@src/f{n}.ts#f{n}"),
            kind: HopKind::Call,
            subtype: None,
            text: format!("function f{n}"),
            evidence: vec![],
            materialized_node: None,
        };
        let hop = |n: usize| UnresolvedHop {
            gap_id: format!("gap:call:vscode@src/c.ts@{n}"),
            source_id: "sym:vscode@src/c.ts#caller".into(),
            edge_label: "CALLS".into(),
            kind: HopKind::Call,
            subtype: None,
            query: format!("call g{n}"),
            evidence: vec![],
            exclusion: None,
        };
        let candidates: Vec<_> = (0..476_907).map(candidate).collect();
        let hops: Vec<_> = (0..81_654).map(hop).collect();
        let texts = embedding_inputs(&hops, &candidates, &[]).unwrap();
        assert_eq!(texts.len(), 558_561);
        let over: Vec<_> = (0..embedding_cache::MAX_ROWS + 1).map(candidate).collect();
        let message = embedding_inputs(&hops, &over, &[]).unwrap_err().to_string();
        assert!(
            message.contains("1048577 candidates (max 1048576)"),
            "{message}"
        );
        // A combined plan past the row bound names the counts that tripped,
        // never a text (#258).
        let wide: Vec<_> = (0..1_040_000).map(candidate).collect();
        let message = embedding_inputs(&hops, &wide, &[]).unwrap_err().to_string();
        assert!(message.contains("1121654 texts (max 1048576)"), "{message}");
        assert!(!message.contains("function f"), "{message}");
    }

    // AC-0285 (T-0285): conservative look-alikes still enter ANN candidacy
    // and produce proposals.
    #[test]
    fn interface_lookalike_hops_stay_in_ann_candidacy() {
        let nodes = vec![
            Node {
                id: "sym:shop@caller.ts#run".into(),
                label: "Symbol".into(),
                props: serde_json::json!({"name": "run", "prov": evidence("caller.ts")}),
            },
            Node {
                id: "gap:call:shop@caller.ts@8".into(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "callee": "Item.getOrder",
                    "reason": "unresolved call target",
                    "prov": evidence("caller.ts")
                }),
            },
            Node {
                id: "sym:shop@item.ts#getOrder".into(),
                label: "Symbol".into(),
                props: serde_json::json!({
                    "name": "Item.getOrder", "prov": evidence("item.ts")
                }),
            },
        ];
        let edges = vec![Edge {
            src: "sym:shop@caller.ts#run".into(),
            dst: "gap:call:shop@caller.ts@8".into(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov": {
                "confidence_tier": "Gap", "evidence": evidence("caller.ts")["evidence"]
            }}),
        }];
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].exclusion, None);
        assert!(hops[0].is_ann_candidate());
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        assert!(!proposals.is_empty());
        assert_eq!(proposals[0].target_id, "sym:shop@item.ts#getOrder");
    }

    // AC-0285 (T-0285): the interface-dispatch detector is conservative —
    // look-alikes stay in candidacy, only `IFoo.member` is excluded.
    #[test]
    fn interface_dispatch_detection_stays_conservative() {
        // Interface dispatch: `I` + capital + at least one more character,
        // then a member access.
        for query in ["IChannel.listen", "INodeProcess.cwd", "IWidget.render"] {
            assert!(
                is_interface_dispatch(query),
                "{query} is interface dispatch"
            );
        }
        // `I` followed by a lowercase letter is a plain capitalized name.
        for query in ["Item.get", "Icon.render"] {
            assert!(!is_interface_dispatch(query), "{query} stays in candidacy");
        }
        // Two-letter receivers are acronyms, not the `IFoo` convention.
        for query in ["IO.read", "ID.parse"] {
            assert!(!is_interface_dispatch(query), "{query} stays in candidacy");
        }
        // A bare type reference is not dispatch; a lower-case receiver is a
        // value, not a type; plain names never match.
        for query in ["IFoo", "channel.listen", "Store.get", "process order"] {
            assert!(!is_interface_dispatch(query), "{query} stays in candidacy");
        }
    }

    // AC-0285 (T-0285): a Gap that stays open in the overlay because its hop
    // is interface dispatch groups under the exclusion reason, the same
    // classification the register applies to the persisted Gap.
    #[test]
    fn interface_dispatch_gap_groups_under_its_exclusion_in_the_overlay() {
        let nodes = vec![
            Node {
                id: "sym:shop@caller.ts#run".into(),
                label: "Symbol".into(),
                props: serde_json::json!({"name": "run", "prov": evidence("caller.ts")}),
            },
            Node {
                id: "gap:call:shop@caller.ts@8".into(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "callee": "IChannel.listen",
                    "reason": "unresolved call target",
                    "prov": evidence("caller.ts")
                }),
            },
        ];
        let edges = vec![Edge {
            src: "sym:shop@caller.ts#run".into(),
            dst: "gap:call:shop@caller.ts@8".into(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov": {
                "confidence_tier": "Gap", "evidence": evidence("caller.ts")["evidence"]
            }}),
        }];
        let report = EvalReport {
            precision_floor: 0.95,
            similarity_threshold: 1.0,
            precision: 0.0,
            recall: 0.0,
            passed: false,
            true_positives: 0,
            false_positives: 0,
            false_negatives: 0,
        };
        let preview = overlay(&nodes, &edges, &[], &report);
        assert_eq!(preview.gaps_filled, 0);
        let gap = preview
            .nodes
            .iter()
            .find(|node| node.label == "Gap")
            .expect("the excluded Gap stays in the overlay");
        let omission = Omission::of_gap(&gap.props, "");
        assert_eq!(omission.detail, HopExclusion::DynamicDispatch.reason());
        assert_eq!(omission.class, OmissionClass::UnresolvedRelationship);
        // Nothing is written onto the Gap; the recorded stop reason stands.
        assert!(gap.props.get("omission").is_none());
        assert_eq!(gap.props["reason"], "unresolved call target");
    }

    // AC-0285 (T-0285): a conservative look-alike (`Item.get`) Gap stays
    // unchanged in the overlay and keeps grouping under its stop reason.
    #[test]
    fn interface_lookalike_gap_stays_untouched_in_the_overlay() {
        let nodes = vec![
            Node {
                id: "sym:shop@caller.ts#run".into(),
                label: "Symbol".into(),
                props: serde_json::json!({"name": "run", "prov": evidence("caller.ts")}),
            },
            Node {
                id: "gap:call:shop@caller.ts@8".into(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "callee": "Item.get",
                    "reason": "unresolved call target",
                    "prov": evidence("caller.ts")
                }),
            },
        ];
        let edges = vec![Edge {
            src: "sym:shop@caller.ts#run".into(),
            dst: "gap:call:shop@caller.ts@8".into(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov": {
                "confidence_tier": "Gap", "evidence": evidence("caller.ts")["evidence"]
            }}),
        }];
        let report = EvalReport {
            precision_floor: 0.95,
            similarity_threshold: 1.0,
            precision: 0.0,
            recall: 0.0,
            passed: false,
            true_positives: 0,
            false_positives: 0,
            false_negatives: 0,
        };
        let preview = overlay(&nodes, &edges, &[], &report);
        let gap = preview
            .nodes
            .iter()
            .find(|node| node.label == "Gap")
            .expect("the look-alike Gap stays in the overlay");
        assert!(gap.props.get("omission").is_none());
        assert_eq!(
            Omission::of_gap(&gap.props, "").detail,
            "unresolved call target"
        );
    }
}
