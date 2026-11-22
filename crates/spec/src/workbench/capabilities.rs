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

/// Knowledge stories validated against one base snapshot, ready to supply to
/// a whole-bundle or selected compilation ([`crate::Supplied::knowledge`]).
/// Built only by [`validate_knowledge_stories`], so every supplied story
/// passed the same checks as the MCP read's (#1443).
pub struct KnowledgeStories<'a>(pub(super) Stories<'a>);

impl std::fmt::Debug for KnowledgeStories<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KnowledgeStories")
            .field("included", &self.0.included.len())
            .field("filtered", &self.0.filtered)
            .finish()
    }
}

impl<'a> KnowledgeStories<'a> {
    /// Arrange the included Capabilities by area (#260, AC-0871). Each
    /// `subsystems` fact is an overlay Subsystem node citing its member
    /// files, current for `base`. A Capability's area is the concept
    /// Subsystem that holds most of the files its cited handlers, or what
    /// its cited entry points and schedules enter (AC-0913), are defined
    /// in; a tie goes to the name that sorts first. A Subsystem that
    /// is malformed, shadows a base fact, is the `shared` group or is
    /// filtered by the export policy arranges nothing, and a Capability
    /// whose handler files are in no Subsystem has no area. The stories
    /// themselves, and every other artifact, are unchanged.
    #[must_use]
    pub fn with_areas(
        mut self,
        base: &'a RevisionedSnapshot,
        subsystems: &'a [core_graph::overlay::OverlayFact],
        mode: ExportMode,
        rejected: &BTreeSet<String>,
    ) -> Self {
        let (arranged, files) = areas(&self.0, base, subsystems, mode, rejected);
        self.0.areas = arranged;
        self.0.file_areas = files;
        self
    }
    /// Included Capabilities that were given an area.
    pub fn arranged(&self) -> usize {
        self.0.areas.len()
    }
    /// Stories the export mode and rejections keep.
    pub fn included(&self) -> usize {
        self.0.included.len()
    }
    /// Stories withheld because they or their support are filtered.
    pub fn filtered(&self) -> usize {
        self.0.filtered
    }
    /// The derived Capabilities and Actors the stories kept, which the
    /// export prints and a draft may cite (#2040).
    pub fn included_nodes(&self) -> impl Iterator<Item = &'a Node> + '_ {
        self.0.included.iter().map(|story| story.node)
    }
}

/// Validate overlay Capability candidates and Actors against `base` under one
/// export policy, as [`compile_knowledge_user_stories`] does, for a host that
/// compiles the whole bundle or a selection rather than `user-stories` alone.
/// `base` must be the snapshot the compilation reads; the caller proves the
/// overlay is current for its revision. Invalid input fails atomically.
pub fn validate_knowledge_stories<'a>(
    base: &'a RevisionedSnapshot,
    candidates: &[CapabilityStory<'a>],
    actors: &'a [core_graph::overlay::OverlayFact],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> Result<KnowledgeStories<'a>, CapabilityStoryInputError> {
    validated_stories(base, candidates, actors, mode, rejected).map(KnowledgeStories)
}

fn areas<'a>(
    stories: &Stories<'a>,
    base: &'a RevisionedSnapshot,
    subsystems: &'a [core_graph::overlay::OverlayFact],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> (BTreeMap<&'a str, &'a Node>, BTreeMap<&'a str, &'a Node>) {
    let base_ids: BTreeSet<&str> = base.graph.0.iter().map(|n| n.id.as_str()).collect();
    // File id → the concept Subsystem it is a member of.
    let mut area_of: BTreeMap<&'a str, &'a Node> = BTreeMap::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for fact in subsystems {
        let core_graph::overlay::OverlayItem::Node(node) = &fact.item else {
            continue;
        };
        let Ok(p) = Provenance::deserialize(&*crate::stored_prov(&node.props)) else {
            continue;
        };
        if node.label != "Subsystem"
            || node.props["kind"] != "cluster"
            || area_name(node).is_empty()
            || base_ids.contains(node.id.as_str())
            || !seen.insert(node.id.as_str())
            || p.validate().is_err()
            || p.tier != Tier::Semantic
            || p.confidence_tier != ConfidenceTier::InferredWeak
            || !matches!(&p.method, ProducingMethod::Rule { .. })
            || !included(&p, mode, rejected)
        {
            continue;
        }
        // The file count the document prints must be the cited membership.
        let members = fact
            .cites
            .iter()
            .filter(|cite| matches!(cite, BaseRef::Node(_)))
            .count();
        if members != fact.cites.len() || node.props["files"].as_u64() != Some(members as u64) {
            continue;
        }
        for cite in &fact.cites {
            if let BaseRef::Node(file) = cite {
                area_of.entry(file.as_str()).or_insert(node);
            }
        }
    }
    if area_of.is_empty() {
        return (BTreeMap::new(), area_of);
    }
    let capabilities = || {
        stories
            .included
            .iter()
            .filter(|story| story.node.label == "Capability")
    };
    // What each Capability's code starts at: the handlers of its cited
    // `HANDLES` edges and, for work no request starts, what its cited entry
    // points and schedules enter. Then the file each is defined in.
    let starts: BTreeSet<&str> = capabilities()
        .flat_map(|story| &story.support)
        .filter_map(|(_, fact)| match fact {
            Support::Node(node) if matches!(node.label.as_str(), "EntryPoint" | "Schedule") => {
                Some(node.id.as_str())
            }
            _ => None,
        })
        .collect();
    let mut entered: BTreeMap<&str, Vec<&'a str>> = BTreeMap::new();
    for edge in &base.graph.1 {
        if edge.label == "ENTRY" && starts.contains(edge.src.as_str()) {
            entered
                .entry(edge.src.as_str())
                .or_default()
                .push(edge.dst.as_str());
        }
    }
    let handlers_of = |story: &Story<'a>| -> Vec<&'a str> {
        story
            .support
            .iter()
            .flat_map(|(_, fact)| match fact {
                Support::Edge(edge) if edge.label == "HANDLES" => vec![edge.dst.as_str()],
                Support::Node(node) => entered.get(node.id.as_str()).cloned().unwrap_or_default(),
                Support::Edge(_) => Vec::new(),
            })
            .collect()
    };
    let handlers: BTreeSet<&str> = capabilities().flat_map(handlers_of).collect();
    let mut file_of: BTreeMap<&str, &'a str> = BTreeMap::new();
    for edge in &base.graph.1 {
        if edge.label == "DEFINED_IN" && handlers.contains(edge.src.as_str()) {
            file_of
                .entry(edge.src.as_str())
                .or_insert(edge.dst.as_str());
        }
    }
    let mut arranged = BTreeMap::new();
    for story in capabilities() {
        let mut files: BTreeSet<&str> = BTreeSet::new();
        let mut held: BTreeMap<(&str, &str), (usize, &'a Node)> = BTreeMap::new();
        for handler in handlers_of(story) {
            // An entry may be the file itself.
            let file = file_of.get(handler).copied().unwrap_or(handler);
            if files.insert(file)
                && let Some(area) = area_of.get(file)
            {
                held.entry((area_name(area), area.id.as_str()))
                    .or_insert((0, area))
                    .0 += 1;
            }
        }
        // Most files first; the map order breaks a tie by name, then id.
        let mut best: Option<(usize, &'a Node)> = None;
        for &(count, area) in held.values() {
            if best.is_none_or(|(most, _)| count > most) {
                best = Some((count, area));
            }
        }
        if let Some((_, area)) = best {
            arranged.insert(story.node.id.as_str(), area);
        }
    }
    (arranged, area_of)
}

pub(crate) enum Support<'a> {
    Node(&'a Node),
    Edge(&'a Edge),
}
pub(crate) struct Story<'a> {
    pub node: &'a Node,
    pub support: Vec<(&'a BaseRef, Support<'a>)>,
}
#[derive(Default)]
pub(crate) struct Stories<'a> {
    pub included: Vec<Story<'a>>,
    pub filtered: usize,
    /// The area of each included Capability, by its node id (#260): the
    /// Subsystem candidate that holds most of its handler files. Empty when
    /// the host supplied no Subsystems; a Capability with no entry has none.
    pub areas: BTreeMap<&'a str, &'a Node>,
    /// The concept Subsystem each member file is in, by File id (#2049):
    /// the same admitted Subsystems `areas` arranges by. Empty when the
    /// host supplied none.
    pub file_areas: BTreeMap<&'a str, &'a Node>,
}
impl<'a> Stories<'a> {
    /// The included Capabilities with their areas, area by area: areas by
    /// name, then the Capabilities with no area, each group in the order
    /// the stories were validated. With no areas that order is unchanged.
    pub(crate) fn capabilities(&self) -> Vec<(Option<&'a Node>, &Story<'a>)> {
        let mut listed: Vec<(Option<&'a Node>, &Story<'a>)> = self
            .included
            .iter()
            .filter(|story| story.node.label == "Capability")
            .map(|story| (self.areas.get(story.node.id.as_str()).copied(), story))
            .collect();
        // A stable sort keeps the validated order inside each area.
        listed.sort_by(|(a, _), (b, _)| match (a, b) {
            (Some(a), Some(b)) => area_name(a).cmp(area_name(b)).then_with(|| a.id.cmp(&b.id)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        listed
    }
}

/// The name a Subsystem candidate was given.
pub(crate) fn area_name(area: &Node) -> &str {
    area.props["name"].as_str().unwrap_or_default()
}

/// Heading under which Capabilities with no area are listed.
pub(crate) const NO_AREA: &str = "Other capabilities";
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
        let p = Provenance::deserialize(&*crate::stored_prov(props))
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
/// Legacy security behavior is unchanged; bounded hosts can also supply admitted flows.
pub fn compile_capability_user_stories(
    base: &RevisionedSnapshot,
    candidates: &[CapabilityStory<'_>],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
) -> Result<SelectedArtifactOutcome, CapabilityStoryInputError> {
    compile_capability_stories(base, candidates, &[], (&[], None), mode, rejected, limits)
}
/// Compile official Capability stories with the caller's complete security admission.
/// A security refusal returns Limited with no artifact, through the shared dispatcher.
/// This compatibility entrypoint retains its empty-flow ADR context.
pub fn compile_capability_user_stories_with_security(
    base: &RevisionedSnapshot,
    candidates: &[CapabilityStory<'_>],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
    security: crate::SecurityLimits,
) -> Result<SelectedArtifactOutcome, CapabilityStoryInputError> {
    compile_capability_stories(
        base,
        candidates,
        &[],
        (&[], Some(security)),
        mode,
        rejected,
        limits,
    )
}
/// Compile official Capability stories with the caller's admitted flows and security limits.
/// Flows participate in shared ADR derivation and its complete output-byte admission.
/// Existing entrypoints retain their empty-flow compatibility behavior.
pub fn compile_capability_user_stories_with_flows_and_security(
    base: &RevisionedSnapshot,
    candidates: &[CapabilityStory<'_>],
    flows: &[flowtracer::Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
    security: crate::SecurityLimits,
) -> Result<SelectedArtifactOutcome, CapabilityStoryInputError> {
    compile_capability_stories(
        base,
        candidates,
        &[],
        (flows, Some(security)),
        mode,
        rejected,
        limits,
    )
}
/// Compile one official artifact with canonical Actors and Capability sidecars.
/// Callers admit retained input/currency, Capability workspace and the separate
/// Actor workspace below. Invalid Actors fail atomically even when hidden.
pub fn compile_knowledge_user_stories(
    base: &RevisionedSnapshot,
    candidates: &[CapabilityStory<'_>],
    actors: &[core_graph::overlay::OverlayFact],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
) -> Result<SelectedArtifactOutcome, CapabilityStoryInputError> {
    compile_capability_stories(
        base,
        candidates,
        actors,
        (&[], None),
        mode,
        rejected,
        limits,
    )
}
/// Compile Actor and Capability sidecars with the caller's admitted flows and
/// complete security limits. Callers also admit inputs, overlay currency and
/// the existing compiler/Actor workspace; this does not connect a host route.
#[allow(clippy::too_many_arguments)]
pub fn compile_knowledge_user_stories_with_flows_and_security(
    base: &RevisionedSnapshot,
    candidates: &[CapabilityStory<'_>],
    actors: &[core_graph::overlay::OverlayFact],
    flows: &[flowtracer::Flow],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
    security: crate::SecurityLimits,
) -> Result<SelectedArtifactOutcome, CapabilityStoryInputError> {
    compile_capability_stories(
        base,
        candidates,
        actors,
        (flows, Some(security)),
        mode,
        rejected,
        limits,
    )
}
fn compile_capability_stories(
    base: &RevisionedSnapshot,
    candidates: &[CapabilityStory<'_>],
    actors: &[core_graph::overlay::OverlayFact],
    context: (&[flowtracer::Flow], Option<crate::SecurityLimits>),
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    limits: SelectedCompileLimits,
) -> Result<SelectedArtifactOutcome, CapabilityStoryInputError> {
    let stories = validated_stories(base, candidates, actors, mode, rejected)?;
    let (flows, security) = context;
    Ok(match security {
        Some(security) => selected::compile_stories_with_flows_and_security(
            base,
            flows,
            mode,
            rejected,
            limits,
            &stories,
            Some(security),
        ),
        None => selected::compile_stories_with_support(base, mode, rejected, limits, &stories),
    })
}

/// Where a story or an Actor is listed: by the name it is shown under, then
/// its id (#2224). Both are borrowed, so the ordering keys own no name bytes.
/// Capabilities and Actors take the plain name `render::NodeName` shows.
pub(crate) type ListedOrder<'a> = (&'a str, &'a str);

fn listed_order(node: &Node) -> ListedOrder<'_> {
    (render::plain_name(node), node.id.as_str())
}

// Returning only borrowed stories drops every lookup/projection/ordering index
// before selected compilation; support Vecs stay owned by the returned stories.
fn validated_stories<'a>(
    base: &'a RevisionedSnapshot,
    candidates: &[CapabilityStory<'a>],
    actors: &'a [core_graph::overlay::OverlayFact],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> Result<Stories<'a>, CapabilityStoryInputError> {
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
    // Stories in name order, the id only breaking a tie: an id hashes the
    // cited facts' ids, which carry the registration's random key
    // (ADR-0023), so id order would renumber every story (US-R-NNNN) each
    // time a source is added again (#2224).
    let mut ids = BTreeSet::new();
    let mut ordered = BTreeMap::new();
    for candidate in candidates {
        let node = candidate.capability;
        let p = Provenance::deserialize(&*crate::stored_prov(&node.props))
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
        if !ids.insert(node.id.as_str()) {
            return Err(CapabilityStoryInputError::DuplicateCandidate);
        }
        ordered.insert(listed_order(node), candidate);
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
        let p = Provenance::deserialize(&*crate::stored_prov(&candidate.capability.props))
            .expect("candidate provenance validated above");
        if included(&p, mode, rejected) && visible {
            stories.included.push(Story {
                node: candidate.capability,
                support,
            });
        } else {
            stories.filtered += 1;
        }
    }
    // Preserve the Capability-only workspace: no Actor index is constructed.
    if actors.is_empty() {
        return Ok(stories);
    }
    let mut actor_ids = BTreeSet::new();
    let mut actor_order = BTreeMap::new();
    for fact in actors {
        let endpoint = crate::knowledge::actors::verify_actor(base, fact)
            .ok_or(CapabilityStoryInputError::Candidate)?;
        let core_graph::overlay::OverlayItem::Node(node) = &fact.item else {
            return Err(CapabilityStoryInputError::Candidate);
        };
        if nodes.contains_key(node.id.as_str()) || ids.contains(node.id.as_str()) {
            return Err(CapabilityStoryInputError::Candidate);
        }
        // Actors by name too, for the same reason as the stories (#2224).
        if !actor_ids.insert(node.id.as_str()) {
            return Err(CapabilityStoryInputError::DuplicateCandidate);
        }
        actor_order.insert(listed_order(node), (node, endpoint, fact));
    }
    for &(node, endpoint, fact) in actor_order.values() {
        let p = Provenance::deserialize(&*crate::stored_prov(&node.props))
            .expect("canonical Actor provenance");
        let support = Support::Node(endpoint);
        if included(&p, mode, rejected)
            && visible_nodes.contains(endpoint.id.as_str())
            && support.visible(mode, rejected)?
        {
            stories.included.push(Story {
                node,
                support: vec![(fact.cites.first().expect("one canonical citation"), support)],
            });
        } else {
            stories.filtered += 1;
        }
    }
    Ok(stories)
}

pub(super) fn support_id(capability: &str, cite: &BaseRef) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"capability-story-support-v1\0");
    serde_json::to_writer(&mut hash, &(capability, cite)).expect("hash writer");
    format!("support:{}", hash.finalize().to_hex())
}

/// Extra Actor-only logical ownership; compose with Capability and ordinary bounds.
/// Retained inputs/output are excluded; output is charged once by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorStoryWorkspaceBound {
    /// Ordered borrowed Actor index, retained Story and singleton support slots.
    pub retained_bytes: usize,
    /// Decoded syntax and canonical full-fact comparison peak.
    pub canonical_scratch_bytes: usize,
    /// Appended references plus current provenance/name and support-hash scratch.
    pub renderer_extra_bytes: usize,
    /// Conservative additive peak; no allocator-capacity or RSS guarantee.
    pub peak_extra_bytes: usize,
}
/// Complete serialized base JSON and Actor OverlayFact JSON bytes bound scratch.
/// Count/byte admission and persistent sessions remain caller responsibilities.
/// Shared Story layout is unchanged, so size_of-based Capability bounds remain valid.
pub fn logical_actor_story_workspace_bound(
    base_json_bytes: usize,
    actor_count: usize,
    actor_json_bytes: usize,
) -> Option<ActorStoryWorkspaceBound> {
    use super::accounting::sum;
    let (index_bytes, story_bytes) = actor_retained_parts(actor_count)?;
    let retained_bytes = sum(&[index_bytes, story_bytes])?;
    let canonical_scratch_bytes =
        crate::knowledge::actors::verification_workspace(base_json_bytes, actor_json_bytes)?;
    let renderer = actor_renderer_parts(base_json_bytes, actor_count, actor_json_bytes)?;
    let renderer_extra_bytes = sum(&[renderer.references_bytes, renderer.current_bytes])?;
    Some(ActorStoryWorkspaceBound {
        retained_bytes,
        canonical_scratch_bytes,
        renderer_extra_bytes,
        peak_extra_bytes: sum(&[
            retained_bytes,
            canonical_scratch_bytes,
            renderer_extra_bytes,
        ])?,
    })
}

fn actor_renderer_parts(
    base_json_bytes: usize,
    actor_count: usize,
    actor_json_bytes: usize,
) -> Option<super::accounting::RendererWorkspaceParts> {
    use super::accounting::{RendererWorkspaceParts, slots, sum};
    Some(RendererWorkspaceParts {
        references_bytes: sum(&[
            slots::<&Node>(actor_count)?,
            // D1 1.2 keeps at most one citation id per Actor, three per
            // caller role, until the role table is written (#2326).
            slots::<String>(actor_count)?,
            actor_count.checked_mul(
                super::citation_id(&core_graph::FactKey::Node { id: String::new() }).len(),
            )?,
        ])?,
        current_bytes: sum(&[
            super::accounting::json_bytes(actor_json_bytes)?, // current provenance Value
            super::accounting::json_bytes(actor_json_bytes)?, // simultaneous typed decode
            super::accounting::json_bytes(base_json_bytes)?,  // current Endpoint provenance Value
            super::accounting::json_bytes(base_json_bytes)?,  // simultaneous typed support decode
            actor_json_bytes,
            base_json_bytes, // current name/id formatting
            std::mem::size_of::<blake3::Hasher>(),
            std::mem::size_of::<String>(), // current support occurrence ID
            support_id("", &BaseRef::Node(String::new())).len(),
        ])?,
    })
}

fn actor_retained_parts(actor_count: usize) -> Option<(usize, usize)> {
    use super::accounting::{map_slots, set_slots, slots, sum};
    use core_graph::overlay::OverlayFact;
    Some((
        sum(&[
            map_slots::<ListedOrder<'_>, (&Node, &Node, &OverlayFact)>(actor_count)?,
            set_slots::<&str>(actor_count)?, // Actor ids, for duplicates
            std::mem::size_of::<BTreeMap<(), ()>>(),
            std::mem::size_of::<BTreeSet<()>>(),
        ])?,
        sum(&[
            slots::<Story<'_>>(actor_count)?,
            slots::<(&BaseRef, Support<'_>)>(actor_count)?,
        ])?,
    ))
}

/// Logical peaks at the validator-return boundary, excluding host-owned inputs.
/// No allocator-capacity or RSS guarantee; all arithmetic is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnowledgeStoryCompileWorkspaceBound {
    /// Existing complete Capability peak, conservatively retained before Actors.
    pub capability_validation_bytes: usize,
    /// Capability lookups/stories, Actor index/stories and current verification.
    pub actor_validation_bytes: usize,
    /// Ordinary compiler and retained stories, after validation indexes drop.
    pub rendering_bytes: usize,
    /// Maximum of the three disjoint stages.
    pub peak_bytes: usize,
}

/// Compose the actual combined story compiler stages under shared sidecar caps.
/// Existing Capability/Actor bounds and caller admission remain unchanged.
pub fn logical_knowledge_story_compile_bound(
    graph: flowtracer::TraceGraphBound,
    trace: flowtracer::TraceLimits,
    compile: SelectedCompileLimits,
    selected_count: usize,
    selected_json_bytes: usize,
) -> Option<KnowledgeStoryCompileWorkspaceBound> {
    use super::accounting::sum;
    let ordinary = super::accounting::logical_story_compile_bound(graph, trace, compile)?;
    let capability = logical_capability_compile_bound(
        graph,
        trace,
        compile,
        selected_count,
        selected_json_bytes,
    )?;
    let actor =
        logical_actor_story_workspace_bound(graph.bytes, selected_count, selected_json_bytes)?;
    let (actor_index, actor_stories) = actor_retained_parts(selected_count)?;
    let capability_validation_bytes = capability.peak_bytes;
    // Visibility can hold Actor and Endpoint decoded provenance together; the
    // unchanged renderer role conservatively covers those decodes/tag buffers.
    let actor_validation_bytes = sum(&[
        capability.lookup_bytes,
        capability.story_support_bytes,
        actor_index,
        actor_stories,
        actor
            .canonical_scratch_bytes
            .max(actor.renderer_extra_bytes),
    ])?;
    let capability_render =
        super::accounting::capability_renderer_parts(selected_count, selected_json_bytes)?;
    let actor_render = actor_renderer_parts(graph.bytes, selected_count, selected_json_bytes)?;
    // Both appended reference roles persist through rendering. Current item
    // scratch ends after each node/support assertion. Prior Provenance DTOs
    // move into output: ordinary.artifact_bytes includes SpecAssertion slots
    // and json_bytes(output cap) for their strings and nested evidence slots
    // (the ownership regression below audits the retained complete shape).
    // Keep two count roles: this helper does not assume a shared total count.
    let rendering_bytes = sum(&[
        ordinary.peak_bytes,
        capability.story_support_bytes,
        actor_stories,
        capability_render.references_bytes,
        actor_render.references_bytes,
        capability_render
            .current_bytes
            .max(actor_render.current_bytes),
        // D1 §1.5 arranges rules per Capability while rendering (AC-0849).
        super::document::rules::rules_per_capability_workspace(graph)?,
        // D1 §1.4 arranges test scenarios per Capability (AC-0850).
        super::document::acceptance::acceptance_per_capability_workspace(graph)?,
        // D1 §1.6 describes each operation's flow per Capability (AC-0859).
        super::document::flows::flows_per_capability_workspace(graph)?,
        // D1 §1.3 and §1.5 state operations and rate limits (AC-0882).
        super::document::operations::operations_per_capability_workspace(graph)?,
        // D1 §1.3 lists the user actions per Capability (AC-0955).
        super::document::actions::actions_per_capability_workspace(graph)?,
    ])?;
    Some(KnowledgeStoryCompileWorkspaceBound {
        capability_validation_bytes,
        actor_validation_bytes,
        rendering_bytes,
        peak_bytes: capability_validation_bytes
            .max(actor_validation_bytes)
            .max(rendering_bytes),
    })
}

#[cfg(test)]
mod stage_tests {
    use super::*;
    use core_graph::overlay::{OverlayFact, OverlayItem};
    use core_prov::{EvidenceRef, ProducingMethod};
    use serde_json::json;

    fn fixture() -> (RevisionedSnapshot, Node, BTreeSet<BaseRef>) {
        let evidence = vec![EvidenceRef {
            repo: "sample".into(),
            path: "orders.ts".into(),
            byte_start: 0,
            byte_end: 1,
            commit_sha: "fixed".into(),
        }];
        let endpoint = Node {
            id: "endpoint:orders".into(),
            label: "Endpoint".into(),
            props: json!({"prov": Provenance::new(Tier::Deterministic,
                ConfidenceTier::Confirmed, evidence.clone(), "t0.adapter-ts", b"endpoint").unwrap()}),
        };
        let cap = Node {
            id: "capability:orders".into(),
            label: "Capability".into(),
            props: json!({"name":"Orders", "prov": Provenance::new(Tier::Semantic,
                ConfidenceTier::InferredWeak, evidence, "test", b"capability").unwrap()
                .with_method(ProducingMethod::Rule {rule:"test".into(), version:"1".into()})}),
        };
        let cites = BTreeSet::from([BaseRef::Node(endpoint.id.clone())]);
        (
            RevisionedSnapshot {
                revision: core_graph::StoreRevision {
                    graph_instance: "stage-test".into(),
                    generation: 1,
                },
                graph: (vec![endpoint], vec![]),
            },
            cap,
            cites,
        )
    }
    fn limits() -> SelectedCompileLimits {
        // Same finite output/ADR limits as the existing Capability bound fixtures.
        SelectedCompileLimits {
            max_bytes: 32768,
            adr: crate::AdrLimits {
                max_rows: 30,
                max_output_bytes: 4096,
            },
        }
    }

    // AC-0518: both canonical Actor and Capability supports borrow source facts,
    // never validator indexes or temporary descriptors, and render after return.
    #[test]
    fn story_support_survives_validator_and_descriptor_scopes() {
        let (base, actors, cap, _) = crate::knowledge::actors::tests::admitted_actor_fixture();
        let cites = &actors[0].cites;
        let OverlayItem::Node(actor) = &actors[0].item else {
            panic!("Actor node")
        };
        let rejected = BTreeSet::new();
        for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
            let stories = {
                let descriptors = [CapabilityStory {
                    capability: &cap,
                    cites,
                }];
                validated_stories(&base, &descriptors, &actors, mode, &rejected).unwrap()
            };
            if mode == ExportMode::BestEffort {
                assert_eq!(stories.included.len(), 2);
                for (story, expected) in stories.included.iter().zip([&cap, actor]) {
                    assert!(std::ptr::eq(story.node, expected));
                    assert_eq!(story.support.len(), 1);
                    let Support::Node(endpoint) = &story.support[0].1 else {
                        panic!("node support")
                    };
                    assert!(std::ptr::eq(*endpoint, &base.graph.0[0]));
                    assert_eq!(story.support[0].0, cites.first().unwrap());
                }
            } else {
                assert!(stories.included.is_empty());
                assert_eq!(stories.filtered, 2);
            }
            let SelectedArtifactOutcome::Produced(rendered) =
                selected::compile_stories_with_support(&base, mode, &rejected, limits(), &stories)
            else {
                panic!("finite fixture must produce an artifact")
            };
            assert!(rendered.verify_identity());
            if mode == ExportMode::BestEffort {
                for node in [&cap, actor] {
                    assert!(
                        rendered
                            .assertions
                            .iter()
                            .any(|row| row.id == format!("node:{}", node.id))
                    );
                    let id = support_id(&node.id, cites.first().unwrap());
                    let support = rendered.assertions.iter().find(|row| row.id == id).unwrap();
                    assert_eq!(support.subject_id, base.graph.0[0].id);
                    assert_eq!(
                        support.provenance,
                        Provenance::deserialize(&base.graph.0[0].props["prov"]).unwrap()
                    );
                }
                assert!(rendered.content.contains("Actor caller set"));
                assert!(rendered.content.contains(
                    "not effective runtime authorization or an identified user, role or principal"
                ));
            } else {
                assert!(!rendered.content.contains("Actor caller set"));
                assert!(rendered.content.contains("2 derived stories omitted"));
            }
            let descriptors = [CapabilityStory {
                capability: &cap,
                cites,
            }];
            let SelectedArtifactOutcome::Produced(combined) = compile_knowledge_user_stories(
                &base,
                &descriptors,
                &actors,
                mode,
                &rejected,
                limits(),
            )
            .unwrap() else {
                panic!("combined fixture must produce")
            };
            assert_eq!(rendered, combined);
            let SelectedArtifactOutcome::Produced(legacy) =
                compile_capability_user_stories(&base, &descriptors, mode, &rejected, limits())
                    .unwrap()
            else {
                panic!("legacy fixture must produce")
            };
            let empty =
                compile_knowledge_user_stories(&base, &descriptors, &[], mode, &rejected, limits())
                    .unwrap();
            assert_eq!(empty, SelectedArtifactOutcome::Produced(legacy));
        }
    }

    // AC-0518: a valid Capability cannot escape as output when a later Actor is
    // malformed, even when export/own curation would otherwise omit that Actor.
    #[test]
    fn invalid_filtered_actor_never_returns_validated_stories() {
        let (base, cap, cites) = fixture();
        let descriptors = [CapabilityStory {
            capability: &cap,
            cites: &cites,
        }];
        let mut actor = cap.clone();
        actor.id = "actor:invalid".into();
        actor.label = "Actor".into();
        let hash = actor.props["prov"]["content_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        let facts = [OverlayFact {
            item: OverlayItem::Node(actor),
            cites: cites.clone(),
        }];
        for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
            for rejected in [BTreeSet::new(), BTreeSet::from([hash.clone()])] {
                assert!(matches!(
                    validated_stories(&base, &descriptors, &facts, mode, &rejected),
                    Err(CapabilityStoryInputError::Candidate)
                ));
                assert!(matches!(
                    compile_knowledge_user_stories(
                        &base,
                        &descriptors,
                        &facts,
                        mode,
                        &rejected,
                        limits()
                    ),
                    Err(CapabilityStoryInputError::Candidate)
                ));
            }
        }
    }

    // AC-0518: checked staged composition retains conservative existing owners
    // and never exceeds their previous fully additive bound.
    #[test]
    fn knowledge_story_stages_preserve_existing_bounds_and_refuse_overflow() {
        let graph = flowtracer::TraceGraphBound {
            bytes: 64,
            rows: 4,
            values: 16,
        };
        let trace = flowtracer::TraceLimits {
            max_flows: 4,
            max_hops: 8,
            max_output_bytes: 512,
        };
        let bound_limits = SelectedCompileLimits {
            max_bytes: 1024,
            adr: crate::AdrLimits {
                max_rows: 8,
                max_output_bytes: 512,
            },
        };
        for (count, bytes) in [(0, 0), (1, 64), (8, 512)] {
            let capability =
                logical_capability_compile_bound(graph, trace, bound_limits, count, bytes).unwrap();
            let actor = logical_actor_story_workspace_bound(graph.bytes, count, bytes).unwrap();
            let staged =
                logical_knowledge_story_compile_bound(graph, trace, bound_limits, count, bytes)
                    .unwrap();
            assert_eq!(staged.capability_validation_bytes, capability.peak_bytes);
            assert!(
                staged.actor_validation_bytes
                    >= actor.retained_bytes + actor.canonical_scratch_bytes
            );
            assert!(
                staged.rendering_bytes
                    >= capability.story_support_bytes + actor.renderer_extra_bytes
            );
            assert_eq!(
                staged.peak_bytes,
                staged
                    .capability_validation_bytes
                    .max(staged.actor_validation_bytes)
                    .max(staged.rendering_bytes)
            );
            assert!(staged.peak_bytes <= capability.peak_bytes + actor.peak_extra_bytes);
            if count == 8 && bytes == 512 {
                assert!(staged.peak_bytes < capability.peak_bytes + actor.peak_extra_bytes);
            }
        }
        assert!(
            logical_knowledge_story_compile_bound(graph, trace, bound_limits, usize::MAX, 1)
                .is_none()
        );
        assert!(
            logical_knowledge_story_compile_bound(graph, trace, bound_limits, 1, usize::MAX)
                .is_none()
        );
    }

    // Owned nested heap only: the inline fields/enum/Vec/String headers of
    // each assertion are paid separately by slots::<SpecAssertion>.
    fn assertion_owned_heap(row: &SpecAssertion) -> usize {
        use core_prov::{ArtifactLocator, EvidenceBasis};
        let SpecAssertion {
            id,
            subject_id,
            subject_kind,
            summary,
            provenance,
            edge_label,
            omission,
        } = row;
        let method = match &provenance.method {
            ProducingMethod::Rule { rule, version } => rule.len() + version.len(),
            ProducingMethod::Model { model } => model.len(),
            ProducingMethod::Human { actor } => actor.len(),
            ProducingMethod::Unrecorded => 0,
        };
        let basis = match &provenance.basis {
            Some(EvidenceBasis::Artifact { refs }) => refs
                .iter()
                .map(|r| {
                    std::mem::size_of::<core_prov::ArtifactRef>()
                        + r.digest.len()
                        + r.kind.len()
                        + match &r.at {
                            ArtifactLocator::Member { path } => path.len(),
                            ArtifactLocator::Offset { .. } => 0,
                        }
                })
                .sum(),
            None | Some(EvidenceBasis::Source | EvidenceBasis::Observation) => 0,
        };
        id.len()
            + subject_id.len()
            + subject_kind.len()
            + summary.len()
            + edge_label.as_ref().map_or(0, String::len)
            + omission.as_ref().map_or(0, |o| o.detail.len())
            + provenance.extractor_id.len()
            + provenance.content_hash.len()
            + method
            + basis
            + provenance
                .evidence
                .iter()
                .map(|e| {
                    std::mem::size_of::<core_prov::EvidenceRef>()
                        + e.repo.len()
                        + e.path.len()
                        + e.commit_sha.len()
                })
                .sum::<usize>()
    }

    // AC-0518/#1557: mixed successful output keeps the exact common assertion
    // budget; reducing accounting overlap never changes admission or content.
    #[test]
    fn mixed_story_renderer_keeps_exact_output_admission() {
        let (base, actors, cap, _) = crate::knowledge::actors::tests::admitted_actor_fixture();
        let candidates = [CapabilityStory {
            capability: &cap,
            cites: &actors[0].cites,
        }];
        let run = |max_bytes| {
            let mut selected = limits();
            selected.max_bytes = max_bytes;
            compile_knowledge_user_stories(
                &base,
                &candidates,
                &actors,
                ExportMode::BestEffort,
                &BTreeSet::new(),
                selected,
            )
            .unwrap()
        };
        let SelectedArtifactOutcome::Produced(artifact) = run(limits().max_bytes) else {
            panic!("mixed fixture must produce")
        };
        assert!(artifact.verify_identity());
        // A JSON value header credit pays each nested collection slot, while
        // output payload strings have their own serialized byte credit.
        let currency = std::mem::size_of::<serde_json::Value>()
            + std::mem::size_of::<String>()
            + 3 * std::mem::size_of::<usize>();
        assert!(std::mem::size_of::<core_prov::EvidenceRef>() <= 6 * currency);
        assert!(std::mem::size_of::<core_prov::ArtifactRef>() <= 5 * currency);
        let retained = artifact.assertions.len() * std::mem::size_of::<SpecAssertion>()
            + artifact
                .assertions
                .iter()
                .map(assertion_owned_heap)
                .sum::<usize>();
        let wire = serde_json::to_vec(&artifact.assertions).unwrap().len();
        assert!(
            retained
                <= artifact.assertions.len() * std::mem::size_of::<SpecAssertion>()
                    + super::super::accounting::json_bytes(wire).unwrap()
        );
        for kind in ["Capability", "Actor"] {
            assert!(
                artifact
                    .assertions
                    .iter()
                    .any(|row| row.subject_kind == kind)
            );
        }
        let bytes =
            artifact.content.len() + serde_json::to_vec(&artifact.assertions).unwrap().len();
        assert!(bytes > 0);
        assert_eq!(run(bytes), SelectedArtifactOutcome::Produced(artifact));
        assert!(matches!(
            run(bytes - 1),
            SelectedArtifactOutcome::Limited { .. }
        ));
    }

    // AC-0518/#1557: legacy totals retain every role; only sequential scratch
    // overlap changes, with persistent references still charged for both caps.
    #[test]
    fn renderer_scratch_split_preserves_legacy_and_strictly_improves_peak() {
        let graph = flowtracer::TraceGraphBound {
            bytes: 64,
            rows: 4,
            values: 16,
        };
        let trace = flowtracer::TraceLimits {
            max_flows: 4,
            max_hops: 8,
            max_output_bytes: 512,
        };
        let selected = SelectedCompileLimits {
            max_bytes: 1024,
            adr: crate::AdrLimits {
                max_rows: 8,
                max_output_bytes: 512,
            },
        };
        let ordinary = logical_compile_bound(graph, trace, selected).unwrap();
        for (count, bytes) in [(0, 0), (1, 64), (8, 512)] {
            let cap =
                logical_capability_compile_bound(graph, trace, selected, count, bytes).unwrap();
            let actor = logical_actor_story_workspace_bound(graph.bytes, count, bytes).unwrap();
            let (_, actor_stories) = actor_retained_parts(count).unwrap();
            let old_render = ordinary.peak_bytes
                + cap.story_support_bytes
                + actor_stories
                + cap.renderer_extra_bytes
                + actor.renderer_extra_bytes;
            let staged =
                logical_knowledge_story_compile_bound(graph, trace, selected, count, bytes)
                    .unwrap();
            let old_peak = cap
                .peak_bytes
                .max(staged.actor_validation_bytes)
                .max(old_render);
            assert!(staged.peak_bytes <= old_peak);
            if count == 8 && bytes == 512 {
                assert!(staged.rendering_bytes < old_render);
                assert!(staged.peak_bytes < old_peak);
            }
        }
        assert!(super::super::accounting::capability_renderer_parts(usize::MAX, 1).is_none());
        assert!(actor_renderer_parts(1, 1, usize::MAX).is_none());
    }

    // AC-0518: the fixed selected route retains base Capability rendering,
    // canonical sidecars, caller flows/security and atomic admission outcomes.
    #[test]
    fn selected_story_route_keeps_base_sidecars_and_admission() {
        let (mut base, actors, cap, flows) =
            crate::knowledge::actors::tests::admitted_actor_fixture();
        let mut base_cap = base.graph.0[0].clone();
        base_cap.id = "base:capability".into();
        base_cap.label = "Capability".into();
        base_cap.props["name"] = json!("Orders <script>\n");
        base.graph.0.push(base_cap);
        let candidates = [CapabilityStory {
            capability: &cap,
            cites: &actors[0].cites,
        }];
        let rejected = BTreeSet::new();
        let security = crate::SecurityLimits {
            max_rows: 64,
            max_output_bytes: 32768,
        };
        let run = |selected, security| {
            compile_knowledge_user_stories_with_flows_and_security(
                &base,
                &candidates,
                &actors,
                &flows,
                ExportMode::BestEffort,
                &rejected,
                selected,
                security,
            )
            .unwrap()
        };
        let SelectedArtifactOutcome::Produced(artifact) = run(limits(), security) else {
            panic!("mixed selected story must produce")
        };
        assert!(artifact.verify_identity());
        for id in ["node:base:capability", "node:cap:admitted"] {
            assert!(artifact.assertions.iter().any(|row| row.id == id));
        }
        assert!(
            artifact
                .assertions
                .iter()
                .any(|row| row.subject_kind == "Actor")
        );
        assert!(artifact.content.contains("Actor caller set"));
        let stories = validated_stories(
            &base,
            &candidates,
            &actors,
            ExportMode::BestEffort,
            &rejected,
        )
        .unwrap();
        assert_eq!(
            SelectedArtifactOutcome::Produced(artifact.clone()),
            selected::compile_stories_with_flows_and_security(
                &base,
                &flows,
                ExportMode::BestEffort,
                &rejected,
                limits(),
                &stories,
                Some(security)
            )
        );
        let mut exact = limits();
        exact.max_bytes =
            artifact.content.len() + serde_json::to_vec(&artifact.assertions).unwrap().len();
        assert_eq!(
            run(exact, security),
            SelectedArtifactOutcome::Produced(artifact)
        );
        exact.max_bytes -= 1;
        assert!(matches!(
            run(exact, security),
            SelectedArtifactOutcome::Limited { .. }
        ));
        assert!(matches!(
            run(
                limits(),
                crate::SecurityLimits {
                    max_rows: 0,
                    max_output_bytes: 0
                }
            ),
            SelectedArtifactOutcome::Limited {
                limit: SelectedArtifactLimit::SecurityDerivation(_),
                ..
            }
        ));
    }

    // AC-0518: narrowing a known selected renderer must strictly remove unrelated
    // workspace without weakening checked limits or changing generic accounting.
    #[test]
    fn selected_story_workspace_is_strictly_smaller_and_checked() {
        let graph = flowtracer::TraceGraphBound {
            bytes: 4096,
            rows: 20,
            values: 100,
        };
        let trace = flowtracer::TraceLimits {
            max_flows: 4,
            max_hops: 8,
            max_output_bytes: 8192,
        };
        let generic = logical_compile_bound(graph, trace, limits()).unwrap();
        let story =
            super::super::accounting::logical_story_compile_bound(graph, trace, limits()).unwrap();
        assert!(story.renderer_workspace_bytes < generic.renderer_workspace_bytes);
        assert!(story.peak_bytes <= generic.peak_bytes);
        assert!(story.artifact_bytes > limits().max_bytes);
        let combined =
            logical_knowledge_story_compile_bound(graph, trace, limits(), 8, 512).unwrap();
        // The pre-Actor stage deliberately retains the complete legacy peak.
        assert!(
            combined.peak_bytes
                >= logical_capability_compile_bound(graph, trace, limits(), 8, 512)
                    .unwrap()
                    .peak_bytes
        );
        let overflow = flowtracer::TraceGraphBound {
            bytes: usize::MAX,
            ..graph
        };
        assert!(
            super::super::accounting::logical_story_compile_bound(overflow, trace, limits())
                .is_none()
        );
        assert!(super::super::producers::logical_story_index_bytes(usize::MAX).is_none());
    }
}
