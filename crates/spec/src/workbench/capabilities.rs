//! Capability-only sidecar inputs; consistency validation is not overlay currency proof.
use super::*;
use core_graph::{RevisionedSnapshot, overlay::BaseRef};
use core_prov::ProducingMethod;

/// A derived Capability and its exact base citations, borrowed from a current overlay.
pub struct CapabilityStory<'a> {
    /// Capability node; base Capability stories remain independent of this input.
    pub capability: &'a Node,
    /// Every base fact used by the producer, never a filtered subset.
    pub cites: &'a BTreeSet<BaseRef>,
}
/// Invalid sidecar input; no partial artifact is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityStoryInputError {
    /// Malformed, duplicate, shadowing or non-Rule Capability.
    Candidate,
    /// A repeated derived Capability identity.
    DuplicateCandidate,
    /// No base citations were provided.
    EmptySupport,
    /// A base reference does not resolve in this snapshot.
    SupportNotInBase,
    /// More than one base fact has the cited identity.
    AmbiguousSupport,
    /// A cited base fact has absent, malformed or invalid provenance.
    InvalidSupportProvenance,
}
impl std::fmt::Display for CapabilityStoryInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid Capability story {:?}", self)
    }
}
impl std::error::Error for CapabilityStoryInputError {}

pub(super) enum Support<'a> {
    Node(&'a Node),
    Edge(&'a Edge),
}
pub(super) struct Story<'a> {
    pub node: &'a Node,
    pub support: Vec<(&'a BaseRef, Support<'a>)>,
}
#[derive(Default)]
pub(super) struct Stories<'a> {
    pub included: Vec<Story<'a>>,
    pub filtered: usize,
}
impl Support<'_> {
    fn visible(
        &self,
        mode: ExportMode,
        rejected: &BTreeSet<String>,
    ) -> Result<bool, CapabilityStoryInputError> {
        let props = match self {
            Self::Node(n) => &n.props,
            Self::Edge(e) => &e.props,
        };
        let p = Provenance::deserialize(&props["prov"])
            .map_err(|_| CapabilityStoryInputError::InvalidSupportProvenance)?;
        p.validate()
            .map_err(|_| CapabilityStoryInputError::InvalidSupportProvenance)?;
        Ok(p.confidence_tier != ConfidenceTier::Gap && included(&p, mode, rejected))
    }
}
/// Compile the official user-stories artifact with exact per-Capability support.
/// Uses the existing selected projection, renderer, identity and output admission.
/// The caller still proves overlay currency and admits its input graph/sidecar.
/// Empty candidates preserve the legacy artifact bytes and identity.
/// Legacy security behavior is unchanged; bounded hosts use the with_security API.
pub fn compile_capability_user_stories(
    base: &RevisionedSnapshot,
    candidates: &[CapabilityStory<'_>],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
) -> Result<SelectedArtifactOutcome, CapabilityStoryInputError> {
    compile_capability_stories(base, candidates, mode, rejected, limits, None)
}
/// Compile official Capability stories with the caller's complete security admission.
/// A security refusal returns Limited with no artifact, through the shared dispatcher.
pub fn compile_capability_user_stories_with_security(
    base: &RevisionedSnapshot,
    candidates: &[CapabilityStory<'_>],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
    security: crate::SecurityLimits,
) -> Result<SelectedArtifactOutcome, CapabilityStoryInputError> {
    compile_capability_stories(base, candidates, mode, rejected, limits, Some(security))
}
fn compile_capability_stories(
    base: &RevisionedSnapshot,
    candidates: &[CapabilityStory<'_>],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
    security: Option<crate::SecurityLimits>,
) -> Result<SelectedArtifactOutcome, CapabilityStoryInputError> {
    let mut nodes = BTreeMap::new();
    for n in &base.graph.0 {
        nodes
            .entry(n.id.as_str())
            .and_modify(|n| *n = None)
            .or_insert(Some(n));
    }
    let mut edges = BTreeMap::new();
    for e in &base.graph.1 {
        edges
            .entry((&e.src, &e.dst, &e.label))
            .and_modify(|e| *e = None)
            .or_insert(Some(e));
    }
    let (visible_nodes, visible_edges) =
        project_recovered_facts(&base.graph.0, &base.graph.1, mode, rejected);
    let visible_nodes: BTreeSet<_> = visible_nodes.iter().map(|n| n.id.as_str()).collect();
    let visible_edges: BTreeSet<_> = visible_edges
        .iter()
        .map(|e| (&e.src, &e.dst, &e.label))
        .collect();
    let mut ordered = BTreeMap::new();
    for candidate in candidates {
        let node = candidate.capability;
        let p = Provenance::deserialize(&node.props["prov"])
            .map_err(|_| CapabilityStoryInputError::Candidate)?;
        if node.label != "Capability"
            || node.id.is_empty()
            || nodes.contains_key(node.id.as_str())
            || p.validate().is_err()
            || p.tier != Tier::Semantic
            || p.confidence_tier != ConfidenceTier::InferredWeak
            || !matches!(&p.method, ProducingMethod::Rule {rule, version} if !rule.is_empty() && !version.is_empty())
        {
            return Err(CapabilityStoryInputError::Candidate);
        }
        if ordered.insert(node.id.as_str(), candidate).is_some() {
            return Err(CapabilityStoryInputError::DuplicateCandidate);
        }
    }
    // Caller admits graph/sidecar input and validation scratch: provenance
    // decoding owns strings even though lookup borrows facts. Overlay admission
    // bounds each citation set. Lookup is O(cites log n), plus provenance decoding;
    // output assertion ownership is separately admitted by the renderer.
    let mut stories = Stories::default();
    for candidate in ordered.values() {
        if candidate.cites.is_empty() {
            return Err(CapabilityStoryInputError::EmptySupport);
        }
        let mut support = Vec::new();
        let mut visible = true;
        for cite in candidate.cites {
            let fact = match cite {
                BaseRef::Node(id) => {
                    let node = nodes
                        .get(id.as_str())
                        .ok_or(CapabilityStoryInputError::SupportNotInBase)?
                        .ok_or(CapabilityStoryInputError::AmbiguousSupport)?;
                    visible &= visible_nodes.contains(node.id.as_str());
                    Support::Node(node)
                }
                BaseRef::Edge(src, dst, label) => {
                    let edge = edges
                        .get(&(src, dst, label))
                        .ok_or(CapabilityStoryInputError::SupportNotInBase)?
                        .ok_or(CapabilityStoryInputError::AmbiguousSupport)?;
                    visible &= visible_edges.contains(&(&edge.src, &edge.dst, &edge.label));
                    Support::Edge(edge)
                }
            };
            visible &= fact.visible(mode, rejected)?;
            support.push((cite, fact));
        }
        let p = Provenance::deserialize(&candidate.capability.props["prov"])
            .expect("candidate provenance validated above");
        if included(&p, mode, rejected) {
            if visible {
                stories.included.push(Story {
                    node: candidate.capability,
                    support,
                });
            } else {
                stories.filtered += 1;
            }
        }
    }
    Ok(match security {
        Some(security) => selected::compile_stories_with_security(
            base,
            mode,
            rejected,
            limits,
            &stories,
            Some(security),
        ),
        None => selected::compile_stories_with_support(base, mode, rejected, limits, &stories),
    })
}

pub(super) fn support_id(capability: &str, cite: &BaseRef) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"capability-story-support-v1\0");
    serde_json::to_writer(&mut hash, &(capability, cite)).expect("hash writer");
    format!("support:{}", hash.finalize().to_hex())
}
