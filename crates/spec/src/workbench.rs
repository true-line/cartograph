//! Deterministic full-spec bundle for the M9 Spec Workbench (US-0012).

use crate::TOPOLOGY_EDGE_LABELS;
pub(crate) mod accounting;
pub use accounting::{
    COMPILE_WORKSPACE_MODEL, CapabilityCompileWorkspaceBound, CompileWorkspaceBound,
    logical_capability_compile_bound, logical_compile_bound,
};
pub(crate) mod architecture;
mod capabilities;
mod catalogue;
pub use catalogue::{
    CATALOGUE_VERSION, CatalogueSection, SectionFrame, SectionKind, catalogue_section,
    catalogue_sections,
};
mod briefing;
pub use briefing::{
    AREA_BRIEFING_BYTES, AREA_SECTIONS, AreaBriefing, AreaNeighbours, area_blocks, area_briefing,
    area_neighbours, briefing_citation_ids,
};
mod configuration;
pub use configuration::CONFIG_READ_COVERAGE;
mod coverage;
pub use coverage::{COVERAGE_DETAIL_REPO_LIMIT, logical_coverage_render_bound};
mod composition;
pub use composition::{
    CompositionFact, CompositionFacts, DerivedGroup, composition_facts, composition_source,
    composition_text, rule_excerpt, rule_excerpt_within, section_text,
};
mod curated;
pub use curated::{
    Curated, CuratedBasis, CuratedFailure, curated_basis_summary, curated_finding_reason_sentence,
    curated_finding_shown, curated_reason_sentence,
};
pub(crate) mod diagrams;
mod document;
mod documentation;
mod manifest;
mod neutrality;
pub use document::{
    SYSTEM_SCOPE, SectionScopeError, cited_fact_digests, cited_fact_hashes, cited_node_hashes,
    drafted_section, section_citation_set,
};
pub use neutrality::{CODE_LABELS, CitedText, SourceIdentifiers, Support, route_literals};
mod endpoint_inventory;
mod endpoint_permissions;
mod environments;
mod external;
pub use endpoint_permissions::{
    ENDPOINT_PATH_MAX_DEPTH, ENDPOINT_PATH_MAX_RELATIONS, ENDPOINT_PATH_MAX_SYMBOLS,
};
pub(crate) mod fact_index;
pub use capabilities::{
    ActorStoryWorkspaceBound, CapabilityStory, CapabilityStoryInputError, KnowledgeStories,
    KnowledgeStoryCompileWorkspaceBound, compile_capability_user_stories,
    compile_capability_user_stories_with_flows_and_security,
    compile_capability_user_stories_with_security, compile_knowledge_user_stories,
    compile_knowledge_user_stories_with_flows_and_security, logical_actor_story_workspace_bound,
    logical_knowledge_story_compile_bound, validate_knowledge_stories,
};
pub use fact_index::{
    FACT_INDEX_SCHEMA, FactIndex, FactIndexError, FactIndexFailure, FactIndexRow, FactRecord,
    FactSpan, SharedText, check_citations, citation_id,
};
mod partition;
mod pipelines;
mod producers;
pub use partition::{
    DATA_PARTITION_EDGE_LABELS, PartitionAcquisition, PartitionArtifact, PartitionArtifactKind,
    PartitionError, PartitionWorkspaceBound, compile_partition_selected, logical_partition_bound,
};
mod registers;
pub(crate) mod render;
pub(crate) mod selected;
use core_graph::omission::{FillSeam, Omission, OmissionClass};
use core_graph::stored::compact_borrowable;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Provenance, Tier};
use flowtracer::{Flow, FlowStatus, Hop};
pub use selected::{
    ArtifactDescriptor, ArtifactSelectionError, SelectedArtifactLimit, SelectedArtifactOutcome,
    SelectedCompileLimits, StaleCoverage, Supplied, artifact_catalog, compile_spec_selected,
    compile_spec_selected_supplied, compile_spec_selected_with_coverage,
    compile_spec_selected_with_security, compile_spec_selected_with_security_supplied,
    project_recovered_facts,
};
use serde::{Deserialize, Serialize};
use std::borrow::{Borrow, Cow};
use std::collections::{BTreeMap, BTreeSet};

/// R-INT-5 projection used by every official artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExportMode {
    /// Confirmed and InferredStrong assertions, with every Gap still listed.
    VerifiedOnly,
    /// Verified-only content plus clearly tagged InferredWeak assertions.
    BestEffort,
}

/// One graph-backed assertion shown with complete inline provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecAssertion {
    /// Stable UI/export identity for this occurrence.
    pub id: String,
    /// Node id or `src label dst` edge identity.
    pub subject_id: String,
    /// Graph node label, edge label, or `FlowHop`.
    pub subject_kind: String,
    /// Human-readable assertion without losing its stable identity.
    pub summary: String,
    /// Producing tier, confidence, evidence, extractor, and content hash.
    pub provenance: Provenance,
    /// The relation an escalation of this gap would need the bounded T3
    /// broker to propose (#238): an edge assertion's own label, a flow hop's
    /// label, or — for a Gap node — the adjacent edge `task_evidence::plan`
    /// (`src-tauri/src/task_evidence/planning.rs`) would resolve the same
    /// way. `None` for a non-gap assertion, or a Gap node with no adjacent
    /// edge (planning then falls back to `CALLS`, which is always allowed).
    /// Lets the register compute its escalation offer from real broker
    /// capability instead of showing it unconditionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_label: Option<String>,
    /// Why a Gap assertion is missing and which seam would fill it (#585,
    /// AC-0254). Present exactly when the provenance is a Gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omission: Option<Omission>,
}

/// The omission a Gap assertion carries: the fact's explicit `omission`, or
/// its stop reason classified (see [`Omission::of_gap`]); `None` for any
/// non-Gap assertion.
pub(crate) fn gap_omission(
    provenance: &Provenance,
    props: &serde_json::Value,
    fallback: &str,
) -> Option<Omission> {
    (provenance.confidence_tier == ConfidenceTier::Gap).then(|| Omission::of_gap(props, fallback))
}

/// One official spec artifact in the deterministic export bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecArtifact {
    /// Stable artifact id used by the Workbench.
    pub id: String,
    /// Portable output file name.
    pub file_name: String,
    /// Display title.
    pub title: String,
    /// `markdown`, `mermaid`, or `json`.
    pub format: String,
    /// Versioned schema id, `cartograph.spec/<id>@<version>` (AC-0242).
    pub schema: String,
    /// Deterministic content identity of the complete artifact (AC-0242):
    /// computed at compile time over schema, id, file name, title, format,
    /// full content and every structured assertion, so a capped preview and
    /// the full artifact carry the same value.
    pub content_id: String,
    /// Complete portable artifact text.
    pub content: String,
    /// Every assertion rendered by the artifact, with inline provenance.
    pub assertions: Vec<SpecAssertion>,
}

/// Version of every artifact's schema. Bump when an artifact's structure
/// changes; the content identity changes with it.
pub const SPEC_ARTIFACT_SCHEMA_VERSION: u32 = 1;

impl SpecArtifact {
    /// The schema id an artifact with this id must carry.
    pub fn expected_schema(id: &str) -> String {
        format!("cartograph.spec/{id}@{SPEC_ARTIFACT_SCHEMA_VERSION}")
    }

    /// Recompute the content identity from the artifact's own fields.
    pub fn compute_content_id(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"cartograph:spec-artifact:v1\0");
        struct HashWriter<'a>(&'a mut blake3::Hasher);
        impl std::io::Write for HashWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        // Preserve the original typed tuple bytes without allocating another
        // complete serialized artifact (including escaped content/assertions).
        serde_json::to_writer(
            HashWriter(&mut hasher),
            &(
                &self.schema,
                &self.id,
                &self.file_name,
                &self.title,
                &self.format,
                &self.content,
                &self.assertions,
            ),
        )
        .expect("spec artifact serializes");
        format!("spec-artifact-v1:{}", hasher.finalize().to_hex())
    }

    /// Whether schema and content identity match the artifact's fields: the
    /// seam a consumer (export, work packet) uses to reject a stale or
    /// altered selection.
    pub fn verify_identity(&self) -> bool {
        self.schema == Self::expected_schema(&self.id)
            && self.content_id == self.compute_content_id()
    }
}

/// Full official spec export returned to the Workbench. `A` is what is kept
/// of each artifact: the artifact itself, or what a caller keeps of it as it
/// is compiled ([`try_compile_spec_supplied_kept`]), so a bundle-sized export
/// need not hold every artifact at once (#2215).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecBundle<A = SpecArtifact> {
    /// R-INT-5 projection applied consistently to all artifacts.
    pub mode: ExportMode,
    /// The complete official artifact set in stable order.
    pub artifacts: Vec<A>,
    /// Number of visible graph-backed assertions across artifacts.
    pub assertion_count: usize,
    /// Explicit unresolved assertions in the Gap register.
    pub gap_count: usize,
    /// Explicit ADR/code conflicts in the Drift register.
    pub drift_count: usize,
    /// Explicit auth/IAM findings in the security view.
    pub security_count: usize,
    /// The graph this bundle was compiled from (#1765, AC-0593). Stamped by
    /// the host, which holds the graph; absent from a bare compile, so those
    /// bytes are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_basis: Option<crate::GraphBasis>,
    /// The curated projection this bundle's `curated` artifact lists
    /// (SPEC-12, AC-0653): its revision and record counts. Stamped by the
    /// host, which computes the projection; absent from a bare compile, so
    /// those bytes are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curated_basis: Option<CuratedBasis>,
}

pub(crate) fn fallback_provenance(identity: &str) -> Provenance {
    Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Gap,
        vec![],
        "spec.invalid-provenance",
        identity.as_bytes(),
    )
    .expect("Gap is within the deterministic confidence ceiling")
}

/// A fact's own `prov` as it was stored. An export's base holds it as its
/// compact JSON text ([`compact_base`]); every spec reader of a fact's own
/// provenance goes through here, so both forms read alike (#2215).
pub use core_graph::stored::stored_prov;

/// A fact's props as they were stored, for a reader that hashes or prints
/// them whole: borrowed unless the base holds the `prov` as text (#2215).
pub fn stored_props(props: &serde_json::Value) -> Cow<'_, serde_json::Value> {
    match stored_prov(props) {
        Cow::Owned(prov) => {
            let mut props = props.clone();
            props["prov"] = prov;
            Cow::Owned(props)
        }
        Cow::Borrowed(_) => Cow::Borrowed(props),
    }
}

/// Restore a fact's props to the form they were stored in, for a fact that
/// leaves the compiler whole, as a view's nodes and edges do (#2215).
pub(crate) fn restore_stored(label: &str, props: &mut serde_json::Value) {
    core_graph::stored::restore_prov(props);
    if label == "BusinessRule"
        && let Cow::Owned(rule) = core_graph::stored::stored(&props["rule"])
    {
        props["rule"] = rule;
    }
}

/// One evidence span of a fact's own `prov`, borrowed from the fact: the
/// fields that are a string or a whole number, as `Value` indexing reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StoredSpan<'a> {
    pub repo: Option<&'a str>,
    pub path: Option<&'a str>,
    pub byte_start: Option<u64>,
}

/// The evidence spans of a fact's own `prov`, borrowed and in order, in
/// either stored form: an empty list when `evidence` is not an array, and one
/// span per element, with no fields for an element that is not an object.
/// Compacted text never needs an escape ([`compact_base`]), so it borrows.
pub(crate) fn stored_spans(props: &serde_json::Value) -> Vec<StoredSpan<'_>> {
    use serde_json::value::RawValue;
    match core_graph::stored::compact_text(&props["prov"]) {
        Some(text) => {
            #[derive(Deserialize)]
            struct Prov<'a> {
                #[serde(borrow, default)]
                evidence: Option<&'a RawValue>,
            }
            let Ok(Prov {
                evidence: Some(evidence),
            }) = serde_json::from_str::<Prov<'_>>(text)
            else {
                return Vec::new();
            };
            let Ok(spans) = serde_json::from_str::<Vec<&RawValue>>(evidence.get()) else {
                return Vec::new();
            };
            spans
                .into_iter()
                .map(|span| {
                    let fields: BTreeMap<&str, &RawValue> =
                        serde_json::from_str(span.get()).unwrap_or_default();
                    let text = |key: &str| {
                        fields
                            .get(key)
                            .and_then(|raw| serde_json::from_str::<&str>(raw.get()).ok())
                    };
                    StoredSpan {
                        repo: text("repo"),
                        path: text("path"),
                        byte_start: fields
                            .get("byte_start")
                            .and_then(|raw| serde_json::from_str::<u64>(raw.get()).ok()),
                    }
                })
                .collect()
        }
        None => props["prov"]["evidence"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|span| StoredSpan {
                repo: span["repo"].as_str(),
                path: span["path"].as_str(),
                byte_start: span["byte_start"].as_u64(),
            })
            .collect(),
    }
}

/// Hold the base of an export that only the spec compiler reads from here on
/// with each fact's own `prov` and each `BusinessRule` payload as compact JSON
/// text. A decoded JSON object costs a B-tree leaf of some 620 bytes however
/// few keys it has, so text is about a sixth of the memory: on REA, `prov`
/// was 1.3 GB and rule payloads 1.0 GB of a 3.0 GB base (#2215). Only objects
/// are rewritten, and only when their text needs no escape, so a reader that
/// borrows strings from the text always can; anything else stays as read.
pub fn compact_base(nodes: &mut [Node], edges: &mut [Edge]) {
    nodes.iter_mut().for_each(compact_node);
    edges.iter_mut().for_each(compact_edge);
}

/// One node of a base held as [`compact_base`] holds it, as it is read.
pub fn compact_node(node: &mut Node) {
    crate::rules::compact_rule_payload(node);
    if let Some(prov) = node.props.get_mut("prov") {
        compact_borrowable(prov);
    }
}

/// One edge of a base held as [`compact_base`] holds it, as it is read.
pub fn compact_edge(edge: &mut Edge) {
    if let Some(prov) = edge.props.get_mut("prov") {
        compact_borrowable(prov);
    }
}

/// A compacted copy of a test's base ([`compact_base`]).
#[cfg(test)]
pub(crate) fn compacted_for_tests(nodes: &[Node], edges: &[Edge]) -> (Vec<Node>, Vec<Edge>) {
    let (mut nodes, mut edges) = (nodes.to_vec(), edges.to_vec());
    compact_base(&mut nodes, &mut edges);
    (nodes, edges)
}

/// A compacted copy of a test's snapshot ([`compact_base`]).
#[cfg(test)]
pub(crate) fn compacted_snapshot_for_tests(
    base: &core_graph::RevisionedSnapshot,
) -> core_graph::RevisionedSnapshot {
    let (nodes, edges) = compacted_for_tests(&base.graph.0, &base.graph.1);
    core_graph::RevisionedSnapshot {
        revision: base.revision.clone(),
        graph: (nodes, edges),
    }
}

/// The validated provenance of a fact, or the explicit-Gap fallback when a
/// fact carries none. Public for the same reason as the register predicates
/// (#116): tier tallies must count with one definition on every surface.
pub fn provenance(props: &serde_json::Value, identity: &str) -> Provenance {
    serde_json::from_value::<Provenance>(stored_prov(props).into_owned())
        .ok()
        .filter(|provenance| provenance.validate().is_ok())
        .unwrap_or_else(|| fallback_provenance(identity))
}

fn is_inferred(confidence: ConfidenceTier) -> bool {
    matches!(
        confidence,
        ConfidenceTier::InferredStrong | ConfidenceTier::InferredWeak
    )
}

pub(crate) fn included(
    provenance: &Provenance,
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> bool {
    if is_inferred(provenance.confidence_tier) && rejected_hashes.contains(&provenance.content_hash)
    {
        return false;
    }
    match provenance.confidence_tier {
        ConfidenceTier::Confirmed | ConfidenceTier::InferredStrong | ConfidenceTier::Gap => true,
        ConfidenceTier::InferredWeak => mode == ExportMode::BestEffort,
    }
}

fn text_prop<'a>(node: &'a Node, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| node.props[*key].as_str())
}

/// The relation a Gap node's escalation would need the bounded T3 broker to
/// propose — mirrors `task_evidence::planning::plan`'s adjacent-slot lookup
/// exactly (dst-first, then src, then the node's own stored `edge_label`
/// prop) so the register's offer and the broker's actual attempt never
/// disagree about which relation is in play (#238). Not a proof of
/// direction/cardinality, same as that lookup. Leaves the final `"CALLS"`
/// default out (unlike planning): `None` here reads as "unknown" to the
/// register, and stays offered rather than being pinned to a guess.
/// The label of the first edge into each node, then of the first edge out
/// of it, indexed once so labelling every gap node is not a scan of every
/// edge per node.
struct GapEdgeLabels<'a> {
    into: BTreeMap<&'a str, &'a str>,
    out_of: BTreeMap<&'a str, &'a str>,
}

impl<'a> GapEdgeLabels<'a> {
    fn new(edges: &[&'a Edge]) -> Self {
        let mut labels = Self {
            into: BTreeMap::new(),
            out_of: BTreeMap::new(),
        };
        for edge in edges {
            labels.into.entry(&edge.dst).or_insert(&edge.label);
            labels.out_of.entry(&edge.src).or_insert(&edge.label);
        }
        labels
    }
}

fn gap_node_edge_label<'a>(node: &'a Node, labels: &GapEdgeLabels<'a>) -> Option<&'a str> {
    let id = node.id.as_str();
    labels
        .into
        .get(id)
        .or_else(|| labels.out_of.get(id))
        .copied()
        .or_else(|| node.props["edge_label"].as_str())
}

pub(crate) fn edge_identity(edge: &Edge) -> String {
    format!("{} {} {}", edge.src, edge.label, edge.dst)
}

/// An artifact whose content is final, stamped with schema and identity.
pub(crate) fn identified_artifact(
    id: &str,
    file_name: &str,
    title: &str,
    format: &str,
    content: String,
    assertions: Vec<SpecAssertion>,
) -> SpecArtifact {
    let mut artifact = SpecArtifact {
        id: id.into(),
        file_name: file_name.into(),
        title: title.into(),
        format: format.into(),
        schema: SpecArtifact::expected_schema(id),
        content_id: String::new(),
        content,
        assertions,
    };
    artifact.content_id = artifact.compute_content_id();
    artifact
}

pub(crate) fn filter_nodes<'a, T: Borrow<Node>>(
    nodes: &'a [T],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> Vec<&'a Node> {
    let mut selected: Vec<&Node> = nodes
        .iter()
        .map(Borrow::borrow)
        .filter(|node| included(&provenance(&node.props, &node.id), mode, rejected_hashes))
        .collect();
    selected.sort_by(|left, right| left.id.cmp(&right.id));
    selected
}

pub(crate) fn filter_edges<'a, T: Borrow<Edge>>(
    edges: &'a [T],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> Vec<&'a Edge> {
    let mut selected: Vec<&Edge> = edges
        .iter()
        .map(Borrow::borrow)
        .filter(|edge| {
            included(
                &provenance(&edge.props, &edge_identity(edge)),
                mode,
                rejected_hashes,
            )
        })
        .collect();
    selected.sort_by(|left, right| {
        (&left.src, &left.dst, &left.label).cmp(&(&right.src, &right.dst, &right.label))
    });
    selected
}

const TRACE_LABELS: &[&str] = &[
    "REALIZES",
    "STEP_OF",
    "GOVERNS",
    "MAPS_TO",
    "DECIDES",
    "TRIGGERED_BY",
    "PERFORMED_BY",
];

/// Group `items` by `key`, largest group first with ties in key order;
/// members keep their input order. Shared by the bounded artifacts (#487).
pub(crate) fn largest_groups<K: Ord, T>(
    items: impl IntoIterator<Item = T>,
    key: impl Fn(&T) -> K,
) -> Vec<(K, Vec<T>)> {
    let mut groups: BTreeMap<K, Vec<T>> = BTreeMap::new();
    for item in items {
        groups.entry(key(&item)).or_default().push(item);
    }
    let mut groups: Vec<(K, Vec<T>)> = groups.into_iter().collect();
    groups.sort_by(|(left_key, left), (right_key, right)| {
        right
            .len()
            .cmp(&left.len())
            .then_with(|| left_key.cmp(right_key))
    });
    groups
}

/// Matrices at or below this many links stay one flat table (#487).
pub const US_TM_FLAT_LIMIT: usize = 200;
/// Relation × target classes detailed in `US-TM.md` past the flat limit;
/// classes past the cap are counted in one explicit line.
pub const US_TM_MAX_CLASSES: usize = 50;
/// Representative links rendered per class in `US-TM.md`.
pub const US_TM_CLASS_REPRESENTATIVES: usize = 5;

fn matrix_class_id(index: usize) -> String {
    format!("M-{:02}", index + 1)
}

const PROJECTION_GAP_REJECTED: &str = "inference rejected by Workbench curation";
const PROJECTION_GAP_VERIFIED_ONLY: &str = "weak inference excluded by verified-only export";

pub(crate) fn project_flows(
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> Vec<Flow> {
    let mut projected: Vec<Flow> = flows
        .iter()
        .map(|flow| {
            let hops: Vec<Hop> = flow
                .hops
                .iter()
                .map(|hop| {
                    if included(&hop.provenance, mode, rejected_hashes) {
                        hop.clone()
                    } else {
                        let reason = if rejected_hashes.contains(&hop.provenance.content_hash) {
                            PROJECTION_GAP_REJECTED
                        } else {
                            PROJECTION_GAP_VERIFIED_ONLY
                        };
                        projection_gap(hop, reason)
                    }
                })
                .collect();
            let has_gap = hops
                .iter()
                .any(|hop| hop.provenance.confidence_tier == ConfidenceTier::Gap);
            let all_confirmed = !hops.is_empty()
                && hops
                    .iter()
                    .all(|hop| hop.provenance.confidence_tier == ConfidenceTier::Confirmed);
            let status = if has_gap || hops.is_empty() || flow.depth_limited {
                FlowStatus::Partial
            } else if all_confirmed {
                FlowStatus::Verified
            } else {
                FlowStatus::Inferred
            };
            let score = if hops.is_empty() {
                0.0
            } else {
                hops.iter()
                    .map(|hop| match hop.provenance.confidence_tier {
                        ConfidenceTier::Confirmed => 1.0,
                        ConfidenceTier::InferredStrong => 0.6,
                        ConfidenceTier::InferredWeak => 0.3,
                        ConfidenceTier::Gap => 0.0,
                    })
                    .sum::<f64>()
                    / hops.len() as f64
            };
            Flow {
                trigger: flow.trigger.clone(),
                trigger_kind: flow.trigger_kind.clone(),
                trigger_name: flow.trigger_name.clone(),
                hops,
                status,
                score,
                depth_limited: flow.depth_limited,
            }
        })
        .collect();
    projected.sort_by(|left, right| left.trigger.cmp(&right.trigger));
    projected
}

/// Replace a suppressed inference with a deterministic unresolved hop. This
/// keeps R-INT-4 intact and prevents filtering from upgrading a partial or
/// inferred flow to Verified.
fn projection_gap(hop: &Hop, reason: &str) -> Hop {
    let canonical = serde_json::to_vec(&(
        "projection-gap",
        &hop.src,
        &hop.label,
        &hop.dst,
        &hop.provenance.content_hash,
        reason,
    ))
    .expect("projection Gap identity serializes");
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Gap,
        vec![],
        "spec.projection-gap",
        &canonical,
    )
    .expect("Gap is within the deterministic confidence ceiling")
    .with_method(crate::spec_rule("spec.projection-gap"));
    Hop {
        label: "UNRESOLVED".into(),
        src: hop.src.clone(),
        dst: format!("gap:projection:{}", &provenance.content_hash[..16]),
        src_name: hop.src_name.clone(),
        dst_name: format!("GAP: {reason}"),
        tier: "Deterministic".into(),
        confidence: "Gap".into(),
        evidence: None,
        provenance,
        gap_reason: Some(reason.into()),
        attempted_tiers: vec![hop.tier.clone()],
    }
}

/// `REFERENCES` joins the data model only where it touches a `DataEntity`
/// (the producer filters on that): JPA relations and repository bindings
/// (#239). Infra `REFERENCES` between resources never reach this view.
pub(crate) const DATA_EDGE_LABELS: &[&str] = &["READS", "WRITES", "MAPS_TO", "REFERENCES"];

/// True when `node` is an explicit System Gap. Public so the shell's
/// findings summary counts with the register's own definition (#116) —
/// every surface must reconcile from one predicate.
pub fn is_gap_node(node: &Node) -> bool {
    node.label == "Gap" || provenance(&node.props, &node.id).confidence_tier == ConfidenceTier::Gap
}

/// True when `edge` is an explicit Gap relation (see [`is_gap_node`]).
pub fn is_gap_edge(edge: &Edge) -> bool {
    provenance(&edge.props, &edge_identity(edge)).confidence_tier == ConfidenceTier::Gap
}

/// One finding per unresolved fact (#241): a Gap-tier edge that touches an
/// explicit gap node supports that node's finding and never doubles it,
/// while an edge-only gap — no gap node on either end, e.g. a callee that
/// cannot be resolved between two real symbols — is a finding of its own.
/// Public for the same reason as the register predicates (#116): every
/// surface must count with one definition.
pub fn count_gap_findings<'a>(
    nodes: impl IntoIterator<Item = &'a Node>,
    edges: impl IntoIterator<Item = &'a Edge>,
) -> usize {
    let gap_ids: BTreeSet<&str> = nodes
        .into_iter()
        .filter(|node| is_gap_node(node))
        .map(|node| node.id.as_str())
        .collect();
    gap_ids.len()
        + edges
            .into_iter()
            .filter(|edge| {
                is_gap_edge(edge)
                    && !gap_ids.contains(edge.src.as_str())
                    && !gap_ids.contains(edge.dst.as_str())
            })
            .count()
}

/// Subset of [`count_gap_findings`] whose classified omission (the same
/// [`gap_omission`] the rendered assertion carries) is filled by
/// [`FillSeam::Interpretation`] and is not an unclassified fallback (#994,
/// ADR-0036). Every other gap finding — including `None` omissions and
/// unclassified fallbacks — stays a system gap, and nothing is subtracted from
/// [`count_gap_findings`].
pub fn count_interpretation_backlog<'a>(
    nodes: impl IntoIterator<Item = &'a Node>,
    edges: impl IntoIterator<Item = &'a Edge>,
) -> usize {
    let is_backlog = |omission: Option<Omission>| {
        matches!(
            omission,
            Some(o) if o.seam == FillSeam::Interpretation && !o.is_unclassified()
        )
    };
    let mut gap_ids = BTreeSet::new();
    let gap_nodes: Vec<&Node> = nodes
        .into_iter()
        .filter(|node| is_gap_node(node) && gap_ids.insert(node.id.as_str()))
        .collect();
    let node_backlog = gap_nodes
        .into_iter()
        .filter(|node| {
            let prov = provenance(&node.props, &node.id);
            is_backlog(gap_omission(
                &prov,
                &node.props,
                &render::NodeName(node).to_string(),
            ))
        })
        .count();
    let edge_backlog = edges
        .into_iter()
        .filter(|edge| {
            is_gap_edge(edge)
                && !gap_ids.contains(edge.src.as_str())
                && !gap_ids.contains(edge.dst.as_str())
        })
        .filter(|edge| {
            let prov = provenance(&edge.props, &edge_identity(edge));
            is_backlog(gap_omission(
                &prov,
                &edge.props,
                &format!("unresolved {} edge", edge.label),
            ))
        })
        .count();
    node_backlog + edge_backlog
}

/// True when `edge` records ADR/code, API contract or schema drift (see
/// [`is_drift_node`]).
pub fn is_drift_edge(edge: &Edge) -> bool {
    matches!(
        edge.label.as_str(),
        "CONFLICTS" | "DRIFTS_FROM" | "DOCUMENTED_AS" | "SERVED_AS" | "MODELED_AS" | "ASSERTED_BY"
    )
}

/// Registers at or below this many rows stay one flat table, as the Gaps
/// lane does (`GROUP_THRESHOLD` in `ui/src/gapClasses.ts`).
pub const GAP_REGISTER_FLAT_LIMIT: usize = 12;
/// Cause classes detailed in `gap_register.md` (#240, AC-0210). Classes past
/// the cap are counted in one explicit line; the sidecar lists them all.
pub const GAP_REGISTER_MAX_CLASSES: usize = 50;
/// Representative instances rendered per class in `gap_register.md`.
pub const GAP_CLASS_REPRESENTATIVES: usize = 5;
/// Cause labels longer than this are shortened in prose only.
const GAP_CAUSE_MAX_CHARS: usize = 240;

/// The escalation rung after the tier that established the gap — mirrors
/// `nextTier` in `ui/src/gapClasses.ts`.
fn next_tier(provenance: &Provenance) -> &'static str {
    match provenance.tier {
        Tier::Dynamic => "T2",
        Tier::Semantic | Tier::Agentic => "T3",
        Tier::Deterministic => "T1",
    }
}

fn class_id(index: usize) -> String {
    format!("C-{:02}", index + 1)
}

/// True when `node` records ADR/code, API contract or schema drift (see
/// [`is_gap_node`] for why these predicates are public).
pub fn is_drift_node(node: &Node) -> bool {
    node.label == "Drift"
        || matches!(
            node.props["kind"].as_str(),
            Some("drift" | "contract-drift" | "schema-drift" | "test-drift")
        )
}

/// Compile the complete official artifact set with one R-INT-5 policy.
/// Rejected inferred content hashes are suppressed without upgrading any fact.
/// No coverage inventory is supplied here, so the two coverage artifacts say
/// so and claim nothing; a store-backed export uses
/// [`compile_spec_with_coverage`].
///
/// # Panics
///
/// When the export's citations do not resolve (see
/// [`try_compile_spec_with_coverage`]).
pub fn compile_spec(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> SpecBundle {
    compile_spec_with_coverage(nodes, edges, flows, mode, rejected_hashes, &[])
}

/// [`compile_spec`] with the coverage inventories stored beside the graph
/// (AC-0592). The `coverage` and `coverage-index` artifacts project exactly
/// these records, in both export modes; without them, as in [`compile_spec`],
/// those two artifacts state that no inventory was supplied.
///
/// # Panics
///
/// When the export's citations do not resolve (see
/// [`try_compile_spec_with_coverage`], which returns that as an error).
pub fn compile_spec_with_coverage(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
    coverage: &[core_graph::coverage::StoredCoverage],
) -> SpecBundle {
    resolved(try_compile_spec_with_coverage(
        nodes,
        edges,
        flows,
        mode,
        rejected_hashes,
        coverage,
    ))
}

/// [`compile_spec_with_coverage`], failing closed instead of panicking when
/// the export's citations do not resolve (ADR-0058 decision 6, AC-0595): an
/// assertion cites an id with no row in the fact index, two different facts
/// share one citation id, or a `verified-only` export cites an InferredWeak
/// fact. No bundle is returned for such an export.
pub fn try_compile_spec_with_coverage(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
    coverage: &[core_graph::coverage::StoredCoverage],
) -> Result<SpecBundle, FactIndexError> {
    try_compile_spec_supplied(
        nodes,
        edges,
        flows,
        mode,
        rejected_hashes,
        Supplied {
            knowledge: None,
            coverage,
            stale_coverage: &[],
            curated: crate::Curated::NotSupplied,
            graph_content_hash: None,
            source_revisions: None,
            repo_names: &[],
        },
    )
}

/// [`try_compile_spec_with_coverage`] with everything the host supplies
/// beside the graph: the stored coverage inventories and the curated
/// projection the `curated` artifact lists (SPEC-12, AC-0653). Without a
/// projection that artifact states that none was supplied. The host stamps
/// [`SpecBundle::curated_basis`]; this function does not.
pub fn try_compile_spec_supplied(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
    supplied: Supplied<'_>,
) -> Result<SpecBundle, FactIndexError> {
    selected::all(
        nodes,
        edges,
        flows,
        mode,
        rejected_hashes,
        supplied,
        |artifact| artifact,
    )
}

/// [`try_compile_spec_supplied`] keeping only `keep(artifact)` of each
/// artifact, called as soon as that artifact is compiled and cited, in
/// bundle order. The artifacts compiled and their counts are the same; an
/// artifact is dropped once `keep` returns, so the bundle is never held
/// whole (#2215).
pub fn try_compile_spec_supplied_kept<A>(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
    supplied: Supplied<'_>,
    keep: impl FnMut(SpecArtifact) -> A,
) -> Result<SpecBundle<A>, FactIndexError> {
    selected::all(nodes, edges, flows, mode, rejected_hashes, supplied, keep)
}

/// The bundle of an export whose citations resolve. The infallible compilers
/// keep their signatures and stop here instead of returning an export with a
/// citation that leads nowhere.
pub(crate) fn resolved<T>(compiled: Result<T, FactIndexError>) -> T {
    compiled.unwrap_or_else(|error| panic!("spec export refused: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graph::FactKey;
    use core_prov::EvidenceRef;

    fn prov(tier: Tier, confidence: ConfidenceTier, hash: &str) -> serde_json::Value {
        serde_json::to_value(
            Provenance::new(
                tier,
                confidence,
                vec![EvidenceRef {
                    repo: "local/shop".into(),
                    path: "src/app.ts".into(),
                    byte_start: 10,
                    byte_end: 42,
                    commit_sha: "abc123".into(),
                }],
                "spec.workbench.test",
                hash.as_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn node(id: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            props: serde_json::json!({
                "name": id,
                "reason": if label == "Gap" { "computed identity" } else { "" },
                "prov": prov(tier, confidence, id),
            }),
        }
    }

    fn edge(src: &str, dst: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: serde_json::json!({"prov": prov(tier, confidence, &edge_identity(&Edge {
                src: src.into(), dst: dst.into(), label: label.into(), props: serde_json::json!({})
            }))}),
        }
    }

    // AC-0587 (#1755): the official `topology.md` path, not only the renderer.
    #[test]
    fn topology_artifact_prints_tiers_and_follows_the_export_mode() {
        use ConfidenceTier as C;
        let nodes = [
            node("res:queue", "Resource", Tier::Deterministic, C::Confirmed),
            node("res:worker", "Resource", Tier::Deterministic, C::Confirmed),
            node("res:guess", "Resource", Tier::Agentic, C::InferredWeak),
            // No valid provenance: the bundle's producers leave Gap-tier
            // facts out of every artifact body, as before this change.
            Node {
                id: "res:unproven".into(),
                label: "Resource".into(),
                props: serde_json::json!({}),
            },
        ];
        let edges = [
            edge(
                "res:queue",
                "res:worker",
                "TRIGGERS",
                Tier::Dynamic,
                C::Confirmed,
            ),
            edge(
                "res:worker",
                "res:queue",
                "DEPENDS_ON",
                Tier::Semantic,
                C::InferredStrong,
            ),
            edge(
                "res:worker",
                "res:guess",
                "REFERENCES",
                Tier::Agentic,
                C::InferredWeak,
            ),
        ];
        let topology = |mode| {
            compile_spec(&nodes, &edges, &[], mode, &BTreeSet::new())
                .artifacts
                .into_iter()
                .find(|artifact| artifact.file_name == "topology.md")
                .unwrap()
                .content
        };
        let best = topology(ExportMode::BestEffort);
        assert!(best.contains(r#"["queue · T0 Confirmed"]"#), "{best}");
        assert!(best.contains("-->|TRIGGERS · T1 Confirmed|"));
        assert!(best.contains("-.->|DEPENDS_ON · T2 InferredStrong|"));
        assert!(best.contains(r#"["guess · T3 InferredWeak"]"#));
        assert!(best.contains("-.->|REFERENCES · T3 InferredWeak|"));
        assert!(!best.contains("unproven"));

        let verified = topology(ExportMode::VerifiedOnly);
        assert!(verified.contains("-->|TRIGGERS · T1 Confirmed|"));
        assert!(verified.contains("-.->|DEPENDS_ON · T2 InferredStrong|"));
        assert!(!verified.contains("InferredWeak"));
        assert!(!verified.contains("res_guess"));
    }

    #[test]
    fn gap_edge_labels_prefer_first_incoming_then_first_outgoing_then_prop() {
        let (t, c) = (Tier::Deterministic, ConfidenceTier::Gap);
        let edges = [
            edge("gap:a", "x", "OUT_A", t, c),
            edge("y", "gap:a", "IN_A", t, c),
            edge("z", "gap:a", "IN_A_LATER", t, c),
            edge("gap:b", "x", "OUT_B", t, c),
            edge("gap:b", "y", "OUT_B_LATER", t, c),
        ];
        let edges: Vec<&Edge> = edges.iter().collect();
        let labels = GapEdgeLabels::new(&edges);
        let a = node("gap:a", "Gap", t, c);
        let b = node("gap:b", "Gap", t, c);
        let mut lone = node("gap:c", "Gap", t, c);
        assert_eq!(gap_node_edge_label(&a, &labels), Some("IN_A"));
        assert_eq!(gap_node_edge_label(&b, &labels), Some("OUT_B"));
        assert_eq!(gap_node_edge_label(&lone, &labels), None);
        lone.props["edge_label"] = "CALLS".into();
        assert_eq!(gap_node_edge_label(&lone, &labels), Some("CALLS"));
    }

    fn flow_hop(confidence: ConfidenceTier, id: &str) -> Hop {
        let provenance: Provenance = serde_json::from_value(prov(
            match confidence {
                ConfidenceTier::InferredStrong => Tier::Semantic,
                ConfidenceTier::InferredWeak => Tier::Agentic,
                _ => Tier::Deterministic,
            },
            confidence,
            id,
        ))
        .unwrap();
        Hop {
            label: "CALLS".into(),
            src: "sym:a".into(),
            dst: id.into(),
            src_name: "a".into(),
            dst_name: id.into(),
            tier: format!("{:?}", provenance.tier),
            confidence: format!("{:?}", confidence),
            evidence: Some("src/app.ts bytes 10..42".into()),
            provenance,
            gap_reason: (confidence == ConfidenceTier::Gap).then(|| "computed identity".into()),
            attempted_tiers: vec![],
        }
    }

    // AC-0258 (T-0258): a projection borrows every selected graph fact and
    // owns only what it derives, so an export never clones the graph (#725).
    #[test]
    fn projection_borrows_graph_facts_and_owns_only_derived_ones() {
        let (mut nodes, edges, flows) = fixture();
        // An explicitly public endpoint derives an owned security finding.
        let mut public = node(
            "ep:public",
            "Endpoint",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        public.props["authenticated"] = false.into();
        nodes.push(public);
        let projection = selected::project(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
            None,
        )
        .unwrap();
        let input: BTreeSet<_> = nodes.iter().map(|node| node.id.as_str()).collect();
        assert!(projection.nodes.iter().any(|n| matches!(n, Cow::Owned(_))));
        for node in &projection.nodes {
            assert_eq!(
                matches!(node, Cow::Borrowed(_)),
                input.contains(node.id.as_str()),
                "{}",
                node.id
            );
        }
        assert!(
            projection
                .edges
                .iter()
                .filter(|edge| edges.contains(edge))
                .all(|edge| matches!(edge, Cow::Borrowed(_)))
        );
    }

    // AC-0258 (T-0258): selected construction and ordinary compatibility.
    #[test]
    fn ordinary_bundle_preserves_frozen_prebounded_bytes() {
        let (nodes, edges, flows) = fixture();
        for (mode, expected) in [
            (
                ExportMode::VerifiedOnly,
                "7ac28564762e59509484c1b6d454e1b81cccdb55312f11ace27496d809dadedb",
            ),
            (
                ExportMode::BestEffort,
                "e22cbb0e52c6ccdb269fc203f961a9a7095f4cf612bf3569832acd749710bf71",
            ),
        ] {
            let bytes = serde_json::to_vec(&graph_artifacts(compile_spec(
                &nodes,
                &edges,
                &flows,
                mode,
                &BTreeSet::new(),
            )))
            .unwrap();
            // Frozen native #603 (03d776d) output, with the already-integrated
            // method column, projection stamping and artifact identity seams,
            // plus the test-arrangements (#946) and journeys artifacts; verified-only
            // journeys state that they withhold InferredWeak candidates. User
            // stories render recovered names, ids and provenance as literal text
            // (#1453), which intentionally changes those bytes, as does
            // security.md stating "no authentication check recovered" and its
            // coverage limit (#1820). data_model.md prints the citation id of
            // each fact it asserts (#1785, ADR-0058 decision 8): a node table,
            // and Confidence and Citation columns on its relation tables. That
            // changes only the data-model artifact's bytes, as does its diagram
            // drawing entities only and its section per entity (#1983). The endpoint
            // permission projection (#1787), the pipeline projection (#1818),
            // the configuration key projection (#1821), the boundary
            // projection (#1788), the environments projection (#1791), the
            // curated items
            // (#1746), the coverage artifacts (#1786) and the fact index
            // (#1785) are appended after these and hashed apart, so the
            // frozen bytes still prove the graph artifacts are unchanged.
            // journey-candidate@2 (ADR-0084) changes only the rule version
            // the journeys artifact prints; with version "1" these bytes match.
            assert_eq!(blake3::hash(&bytes).to_hex().as_str(), expected, "{mode:?}");
        }
    }

    /// The bundle without the endpoint permission projection (#1787), the
    /// pipeline projection (#1818), the configuration key projection
    /// (#1821), the boundary projection (#1788), the environments projection
    /// (#1791), the documentation evidence projection (#1750), the endpoint
    /// inventory (#1658), the curated items, which list a projection the host
    /// supplies (#1746), the two coverage artifacts, which project stored
    /// inventories instead of the graph (#1786), the bundle manifest, which
    /// lists the files, and the two fact-index artifacts, which
    /// list what the others cite (#1785), and the D1 document (#1748), which
    /// cites the facts its frames list. The sixteen close the official order;
    /// the fixture has no endpoint, no pipeline, no environment file, no
    /// configuration read, no outbound call, no observed client, no channel,
    /// no Compose file and no documentation file, and a bare compile has no curated projection, so
    /// none carries an assertion here.
    fn graph_artifacts(mut bundle: SpecBundle) -> SpecBundle {
        let appended = bundle.artifacts.split_off(bundle.artifacts.len() - 16);
        assert_eq!(
            appended
                .iter()
                .map(|artifact| artifact.id.as_str())
                .collect::<Vec<_>>(),
            [
                "endpoint-permissions",
                "pipelines",
                "configuration",
                "external",
                "environments",
                "documentation-evidence",
                "endpoint-inventory",
                "curated",
                "architecture",
                "architecture-dsl",
                "d1-product-specification",
                "coverage",
                "coverage-index",
                "bundle-manifest",
                "fact-index",
                "fact-index-json"
            ]
        );
        // The D1 document cites the facts its frames list (AC-0663), and
        // the architecture the facts its boxes and arrows rest on (AC-0940).
        assert!(
            appended
                .iter()
                .filter(|artifact| {
                    !["d1-product-specification", "architecture"].contains(&artifact.id.as_str())
                })
                .all(|artifact| artifact.assertions.is_empty())
        );
        // Its assertions are counted apart, so the frozen bytes still prove
        // the graph artifacts and their count are unchanged.
        bundle.assertion_count -= appended
            .iter()
            .map(|artifact| artifact.assertions.len())
            .sum::<usize>();
        bundle
    }

    fn fixture() -> (Vec<Node>, Vec<Edge>, Vec<Flow>) {
        let nodes = vec![
            node(
                "cap:orders",
                "Capability",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "data:orders",
                "DataEntity",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "domain:order",
                "DomainEntity",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
            node(
                "sym:save-order",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "adr:queue",
                "ADR",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
            node(
                "gap:channel",
                "Gap",
                Tier::Deterministic,
                ConfidenceTier::Gap,
            ),
            node(
                "drift:queue",
                "Drift",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
            node(
                "weak:rule",
                "BusinessRule",
                Tier::Agentic,
                ConfidenceTier::InferredWeak,
            ),
        ];
        let edges = vec![
            edge(
                "domain:order",
                "data:orders",
                "MAPS_TO",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
            edge(
                "sym:save-order",
                "data:orders",
                "WRITES",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            edge(
                "adr:queue",
                "cap:orders",
                "DECIDES",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
        ];
        let flows = vec![Flow {
            trigger: "ep:orders".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "POST /orders".into(),
            hops: vec![
                flow_hop(ConfidenceTier::Confirmed, "sym:confirmed"),
                flow_hop(ConfidenceTier::InferredWeak, "sym:weak"),
                flow_hop(ConfidenceTier::Gap, "gap:channel"),
            ],
            status: FlowStatus::Partial,
            score: 0.43,
            depth_limited: false,
        }];
        (nodes, edges, flows)
    }

    #[test]
    fn full_bundle_has_every_official_artifact_with_inline_provenance() {
        // AC-0032 / AC-0035 (T-0032, T-0035).
        let (nodes, edges, flows) = fixture();
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let names: Vec<&str> = bundle
            .artifacts
            .iter()
            .map(|artifact| artifact.file_name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "user_stories.md",
                "US-TM.md",
                "US-TM.json",
                "flow_dossiers.md",
                "topology.md",
                "data_model.md",
                "adrs.md",
                "gap_register.md",
                "gap_register.json",
                "drift_register.md",
                "security.md",
                "toolchain.md",
                "test_arrangements.md",
                "journeys.md",
                "rule-evidence.md",
                "rule-evidence.json",
                "endpoint-permissions.md",
                "pipelines.md",
                "configuration.md",
                "external.md",
                "environments.md",
                "documentation-evidence.md",
                "endpoint-inventory.md",
                "curated.md",
                "architecture.md",
                "architecture.dsl",
                "product-specification.md",
                "coverage.md",
                "coverage.json",
                "bundle-manifest.md",
                "fact-index.md",
                "fact-index.json",
            ]
        );
        // The coverage artifacts state facts about the run, not graph
        // assertions, so they carry no provenance block (#1786); the fact
        // index is that provenance, listed once per fact (#1785).
        for artifact in &graph_artifacts(bundle.clone()).artifacts {
            assert_eq!(
                artifact
                    .content
                    .contains("Assertions and inline provenance"),
                artifact.format != "json",
                "{}",
                artifact.file_name
            );
            for assertion in &artifact.assertions {
                assert!(!assertion.provenance.extractor_id.is_empty());
                assert!(!assertion.provenance.content_hash.is_empty());
            }
        }
        let topology = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "topology")
            .unwrap();
        assert_eq!(topology.format, "markdown");
        assert!(topology.content.contains("```mermaid\nflowchart LR"));
        assert!(
            topology
                .content
                .contains("Assertions and inline provenance")
        );

        let data_model = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "data-model")
            .unwrap();
        assert!(data_model.content.contains("domain:order"));
        assert!(data_model.content.contains("sym:save-order"));
        assert!(data_model.content.contains("MAPS_TO"));
        assert!(data_model.content.contains("WRITES"));
        // One gap node → one displayed finding; the register's flow-hop
        // restatement of the same gap stays a supporting row (#241).
        assert_eq!(bundle.gap_count, 1);
        assert_eq!(bundle.drift_count, 1);
        assert_eq!(bundle.security_count, 0);
    }

    #[test]
    fn data_model_renders_entity_relations_and_repository_access() {
        // AC-0284 (#239): REFERENCES between entities (JPA relations) and
        // from a repository Symbol to its entity appear in data_model.md
        // with READS/WRITES; an infra REFERENCES edge stays out.
        let nodes = vec![
            node(
                "data:owner",
                "DataEntity",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "data:pet",
                "DataEntity",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:save-pet",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "res:a",
                "Resource",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "res:b",
                "Resource",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
        ];
        let edges = vec![
            edge(
                "data:owner",
                "data:pet",
                "REFERENCES",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            edge(
                "sym:save-pet",
                "data:pet",
                "WRITES",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            edge(
                "res:a",
                "res:b",
                "REFERENCES",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
        ];
        let bundle = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let data = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "data-model")
            .unwrap();
        assert!(
            data.content
                .contains("| `data:owner` | REFERENCES | `data:pet` |")
        );
        assert!(
            data.content
                .contains("| `sym:save-pet` | WRITES | `data:pet` |")
        );
        assert!(!data.content.contains("res:a"), "{}", data.content);
        assert!(data.assertions.len() >= 5);
    }

    // AC-0858: the diagram draws entities only; each entity has a section
    // with its table, columns, related entities and its writers by name.
    #[test]
    fn data_model_has_a_section_per_entity_and_draws_entities_only() {
        use serde_json::json;
        let mut owner = node(
            "data:app@sqlalchemy:owners",
            "DataEntity",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        owner.props["name"] = json!("Owner");
        owner.props["table"] = json!("owners");
        let mut pet = node(
            "data:app@sqlalchemy:pets",
            "DataEntity",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        pet.props["name"] = json!("Pet");
        pet.props["table"] = json!("pets");
        pet.props["columns"] = json!([
            {"name": "id", "type": "Integer", "primary_key": true},
            {"name": "owner_id", "type": "Integer", "nullable": false},
        ]);
        let mut save = node(
            "sym:app@pets/repo.py#PetRepository.save",
            "Symbol",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        save.props["name"] = json!("save");
        let nodes = vec![owner, pet, save];
        let references = edge(
            "data:app@sqlalchemy:pets",
            "data:app@sqlalchemy:owners",
            "REFERENCES",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let mut writes = edge(
            "sym:app@pets/repo.py#PetRepository.save",
            "data:app@sqlalchemy:pets",
            "WRITES",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        writes.props["operations"] = json!(["insert", "update"]);
        // A repository bound to its entity is not a related entity.
        let binding = edge(
            "sym:app@pets/repo.py#PetRepository.save",
            "data:app@sqlalchemy:pets",
            "REFERENCES",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let edges = vec![references.clone(), writes, binding];
        let bundle = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let data = &bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "data-model")
            .unwrap()
            .content;
        let diagram = &data[..data.find("```\n").unwrap()];
        assert!(diagram.contains("[\"Pet\"]"), "{diagram}");
        assert!(diagram.contains("-->|REFERENCES|"), "{diagram}");
        assert!(!diagram.contains("Symbol"), "{diagram}");
        assert!(!diagram.contains("WRITES"), "{diagram}");
        let pets =
            &data[data.find("\n### Pet\n").unwrap()..data.find("## Entities and related").unwrap()];
        let cite = |key: FactKey| crate::workbench::fact_index::citation_id(&key);
        for line in [
            format!(
                "Stored in `pets`. Cites {}.",
                cite(FactKey::Node {
                    id: "data:app@sqlalchemy:pets".into()
                })
            ),
            "| `id` | Integer | primary |  |".to_owned(),
            "| `owner_id` | Integer |  | false |".to_owned(),
            format!(
                "- **References:** Owner ({})",
                cite(FactKey::Edge {
                    source: references.src.clone(),
                    label: references.label.clone(),
                    destination: references.dst.clone(),
                })
            ),
            "- **Written by** (1 callables): insert/update: `PetRepository.save`".to_owned(),
        ] {
            assert!(pets.contains(&line), "{line}\n{pets}");
        }
        assert!(!pets.contains("Referenced by"), "{pets}");
        let owners = &data[data.find("\n### Owner\n").unwrap()..data.find("\n### Pet\n").unwrap()];
        assert!(owners.contains("- **Referenced by:** Pet ("), "{owners}");
    }

    // AC-0847: the data model lists each entity's own columns, cited.
    #[test]
    fn data_model_lists_each_entity_column() {
        // A rebuild needs the columns, not only entity names: `columns`
        // (SQLAlchemy, TypeORM) and `fields` (JPA) both render, each row
        // citing its entity.
        let mut product = node(
            "data:product",
            "DataEntity",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        product.props["name"] = "Product".into();
        product.props["columns"] = serde_json::json!([
            {"name": "id", "type": "number", "primary_key": true},
            {"name": "deletedAt", "type": "Date | null", "nullable": true, "column": "deleted_at"},
        ]);
        let mut ticket = node(
            "data:ticket",
            "DataEntity",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        ticket.props["name"] = "Ticket".into();
        ticket.props["fields"] = serde_json::json!([{"name": "id", "type": "Long", "id": true}]);
        let bundle = compile_spec(
            &[product, ticket],
            &[],
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let data = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "data-model")
            .unwrap();
        let cite = |id: &str| citation_id(&FactKey::Node { id: id.to_owned() });
        let content = &data.content;
        assert!(content.contains("## Entity columns"), "{content}");
        assert!(
            content.contains(&format!(
                "| Product | `id` | number | primary |  |  | {} |",
                cite("data:product")
            )),
            "{content}"
        );
        assert!(
            content.contains(&format!(
                "| Product | `deletedAt` | Date \\| null |  | true | deleted_at | {} |",
                cite("data:product")
            )),
            "{content}"
        );
        assert!(
            content.contains(&format!(
                "| Ticket | `id` | Long | primary |  |  | {} |",
                cite("data:ticket")
            )),
            "{content}"
        );
    }

    #[test]
    fn data_model_lists_every_relation_field_of_an_aggregated_relation() {
        // AC-0284 (#239, #702 review): several relation fields to the same
        // target entity are one fact — one edge — so data_model.md lists
        // every field that edge declares, not one field and not one row per
        // field.
        let nodes = vec![
            node(
                "data:ticket",
                "DataEntity",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "data:user",
                "DataEntity",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
        ];
        let mut relation = edge(
            "data:ticket",
            "data:user",
            "REFERENCES",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        relation.props["relations"] = serde_json::json!([
            {
                "field": "approvedBy",
                "relation": "OneToOne",
                "cardinality": "one-to-one",
                "owning": true,
            },
            {
                "field": "createdBy",
                "relation": "ManyToOne",
                "cardinality": "many-to-one",
                "owning": true,
            },
        ]);
        let bundle = compile_spec(
            &nodes,
            &[relation],
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let data = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "data-model")
            .unwrap();
        // The edge stays one row…
        assert!(
            data.content
                .contains("| `data:ticket` | REFERENCES | `data:user` |")
        );
        // …and every field it declares is listed.
        for field in [
            "| `data:ticket` | `approvedBy` | OneToOne | one-to-one | true | `data:user` |",
            "| `data:ticket` | `createdBy` | ManyToOne | many-to-one | true | `data:user` |",
        ] {
            assert!(data.content.contains(field), "{field}: {}", data.content);
        }
    }

    /// The citation ids `text` prints: `F-` and 16 hex characters.
    fn printed_citation_ids(text: &str) -> BTreeSet<&str> {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .filter(|word| word.len() == 18 && word.starts_with("F-"))
            .collect()
    }

    fn data_model_fixture() -> (Vec<Node>, Vec<Edge>) {
        use ConfidenceTier as C;
        let nodes = vec![
            node(
                "data:owner",
                "DataEntity",
                Tier::Deterministic,
                C::Confirmed,
            ),
            node("data:pet", "DataEntity", Tier::Deterministic, C::Confirmed),
            node("data:guess", "DataEntity", Tier::Semantic, C::InferredWeak),
            node("sym:save-pet", "Symbol", Tier::Deterministic, C::Confirmed),
        ];
        let mut relation = edge(
            "data:owner",
            "data:pet",
            "REFERENCES",
            Tier::Deterministic,
            C::Confirmed,
        );
        relation.props["relations"] = serde_json::json!([{
            "field": "pets",
            "relation": "OneToMany",
            "cardinality": "one-to-many",
            "owning": false,
        }]);
        let mut weak_relation = edge(
            "data:owner",
            "data:guess",
            "REFERENCES",
            Tier::Semantic,
            C::InferredWeak,
        );
        weak_relation.props["relations"] = serde_json::json!([{
            "field": "guess",
            "relation": "ManyToOne",
            "cardinality": "many-to-one",
            "owning": true,
        }]);
        let edges = vec![
            relation,
            edge(
                "sym:save-pet",
                "data:pet",
                "WRITES",
                Tier::Deterministic,
                C::Confirmed,
            ),
            edge(
                "sym:save-pet",
                "data:guess",
                "WRITES",
                Tier::Semantic,
                C::InferredWeak,
            ),
            weak_relation,
        ];
        (nodes, edges)
    }

    // AC-0760 (#1785, ADR-0058 decision 8): data_model.md prints the
    // citation id of every fact it asserts, the ids it prints are exactly
    // those facts, each resolves in the export's fact index, and a relation
    // field cites its relation edge.
    #[test]
    fn data_model_prints_a_resolving_citation_for_each_statement() {
        let (nodes, edges) = data_model_fixture();
        for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
            let bundle = compile_spec(&nodes, &edges, &[], mode, &BTreeSet::new());
            let data = bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "data-model")
                .unwrap();
            let index: FactIndex = serde_json::from_str(
                &bundle
                    .artifacts
                    .iter()
                    .find(|artifact| artifact.id == "fact-index-json")
                    .unwrap()
                    .content,
            )
            .unwrap();
            let cited: BTreeSet<String> = index
                .rows
                .iter()
                .filter(|row| row.cited_by.iter().any(|by| by == "data-model"))
                .map(|row| row.citation_id.clone())
                .collect();
            assert_eq!(cited.len(), data.assertions.len(), "{mode:?}");
            let printed = printed_citation_ids(&data.content);
            assert_eq!(
                printed,
                cited.iter().map(String::as_str).collect::<BTreeSet<_>>(),
                "{mode:?}: {}",
                data.content
            );
            check_citations(&index, printed.iter().copied()).expect("every id resolves");

            let relation = citation_id(&FactKey::Edge {
                source: "data:owner".into(),
                label: "REFERENCES".into(),
                destination: "data:pet".into(),
            });
            assert!(data.content.contains(&format!(
                "| `data:owner` | REFERENCES | `data:pet` | Confirmed | {relation} |"
            )));
            assert!(data.content.contains(&format!(
                "| `data:owner` | `pets` | OneToMany | one-to-many | false | `data:pet` | Confirmed | {relation} |"
            )));
            let pet = citation_id(&FactKey::Node {
                id: "data:pet".into(),
            });
            assert!(data.content.contains(&format!(
                "| `data:pet` | DataEntity | Deterministic | Confirmed | {pet} |"
            )));
        }
    }

    // AC-0760 (#1785, R-INT-5): a verified-only data_model.md prints no
    // citation of an InferredWeak fact and its index has no row for one; a
    // best-effort one prints them with their confidence, and both resolve.
    #[test]
    fn data_model_citations_follow_the_export_mode() {
        let (nodes, edges) = data_model_fixture();
        let weak_entity = citation_id(&FactKey::Node {
            id: "data:guess".into(),
        });
        let weak_write = citation_id(&FactKey::Edge {
            source: "sym:save-pet".into(),
            label: "WRITES".into(),
            destination: "data:guess".into(),
        });
        let content = |mode| {
            compile_spec(&nodes, &edges, &[], mode, &BTreeSet::new())
                .artifacts
                .into_iter()
                .find(|artifact| artifact.id == "data-model")
                .unwrap()
                .content
        };
        let verified = content(ExportMode::VerifiedOnly);
        assert!(!verified.contains(&weak_entity));
        assert!(!verified.contains(&weak_write));
        assert!(!verified.contains("InferredWeak"));
        let best = content(ExportMode::BestEffort);
        assert!(best.contains(&format!(
            "| `data:guess` | DataEntity | Semantic | InferredWeak | {weak_entity} |"
        )));
        assert!(best.contains(&format!(
            "| `sym:save-pet` | WRITES | `data:guess` | InferredWeak | {weak_write} |"
        )));
        // A relation field prints its relation edge's confidence and citation.
        let weak_relation = citation_id(&FactKey::Edge {
            source: "data:owner".into(),
            label: "REFERENCES".into(),
            destination: "data:guess".into(),
        });
        assert!(!verified.contains(&weak_relation));
        assert!(best.contains(&format!(
            "| `data:owner` | `guess` | ManyToOne | many-to-one | true | `data:guess` | InferredWeak | {weak_relation} |"
        )));
        // Deterministic: the same graph prints the same bytes.
        assert_eq!(best, content(ExportMode::BestEffort));
        assert_eq!(verified, content(ExportMode::VerifiedOnly));
    }

    #[test]
    fn verified_only_excludes_weak_but_preserves_gap_and_drift_registers() {
        // AC-0034 / R-INT-5 (T-0034).
        let (nodes, edges, flows) = fixture();
        let verified = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let dossier = verified
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "flow-dossiers")
            .unwrap();
        assert!(!dossier.content.contains("sym:weak"));
        assert!(dossier.content.contains("gap:channel"));
        assert!(
            dossier
                .content
                .contains("weak inference excluded by verified-only export")
        );
        assert!(!dossier.content.contains("— Verified"));
        // Still one gap node: verified-only adds excluded-weak flow rows to
        // the register, but supporting assertions never inflate the count.
        assert_eq!(verified.gap_count, 1);
        assert_eq!(verified.drift_count, 1);
        assert_eq!(verified.security_count, 0);
    }

    #[test]
    fn rejected_inference_is_suppressed_without_upgrading_other_facts() {
        // AC-0033: curation suppresses by stable content hash.
        let (nodes, edges, flows) = fixture();
        let rejected_hash = flows[0].hops[1].provenance.content_hash.clone();
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::from([rejected_hash]),
        );
        let dossier = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "flow-dossiers")
            .unwrap();
        assert!(!dossier.content.contains("sym:weak"));
        assert!(dossier.content.contains("sym:confirmed"));
        assert!(
            dossier
                .content
                .contains("inference rejected by Workbench curation")
        );
    }

    #[test]
    fn compile_derives_inferred_adr_and_flow_mapped_drift() {
        // AC-0037 / AC-0038 (T-0037, T-0038): compilation keeps recovered
        // decisions inferred and maps explicit conflicts to edge and flow.
        let nodes = vec![
            node(
                "chan:orders",
                "Channel",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:publish",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:handler",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:remote",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            Node {
                id: "adr:found:no-sync".into(),
                label: "ADR".into(),
                props: serde_json::json!({
                    "title": "No synchronous calls",
                    "status": "Accepted",
                    "origin": "found",
                    "forbids": ["CALLS"],
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "found-adr"),
                }),
            },
        ];
        let edges = vec![
            edge(
                "sym:publish",
                "chan:orders",
                "PUBLISHES",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            edge(
                "adr:found:no-sync",
                "sym:handler",
                "DECIDES",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            edge(
                "sym:handler",
                "sym:remote",
                "CALLS",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
        ];
        let mut call = flow_hop(ConfidenceTier::Confirmed, "sym:remote");
        call.src = "sym:handler".into();
        call.src_name = "handler".into();
        let flows = vec![Flow {
            trigger: "ep:orders".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "POST /orders".into(),
            hops: vec![call],
            status: FlowStatus::Verified,
            score: 1.0,
            depth_limited: false,
        }];
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let adrs = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "adrs")
            .unwrap();
        let recovered = adrs
            .assertions
            .iter()
            .find(|assertion| assertion.subject_id.starts_with("adr:recovered:"))
            .unwrap();
        assert_eq!(recovered.provenance.tier, Tier::Semantic);
        assert_eq!(
            recovered.provenance.confidence_tier,
            ConfidenceTier::InferredStrong
        );
        let drift = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "drift-register")
            .unwrap();
        assert!(drift.content.contains("sym:handler CALLS sym:remote"));
        assert!(drift.content.contains("ep:orders"));
        assert_eq!(bundle.drift_count, 1);
    }

    #[test]
    fn rejected_support_cannot_derive_an_adr_or_drift_finding() {
        // AC-0037/AC-0038 (T-0037/T-0038): Workbench rejection is applied
        // before derivation, so a new derived hash cannot bypass curation.
        let nodes = vec![
            node(
                "chan:orders",
                "Channel",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:publish",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:handler",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:remote",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            Node {
                id: "adr:found:no-sync".into(),
                label: "ADR".into(),
                props: serde_json::json!({
                    "title": "No synchronous calls",
                    "origin": "found",
                    "forbids": ["CALLS"],
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "found-adr"),
                }),
            },
        ];
        let publish = edge(
            "sym:publish",
            "chan:orders",
            "PUBLISHES",
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
        );
        let decides = edge(
            "adr:found:no-sync",
            "sym:handler",
            "DECIDES",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let call = edge(
            "sym:handler",
            "sym:remote",
            "CALLS",
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
        );
        let rejected_hashes = BTreeSet::from([
            provenance(&publish.props, "publish").content_hash,
            provenance(&call.props, "call").content_hash,
        ]);

        let bundle = compile_spec(
            &nodes,
            &[publish, decides, call],
            &[],
            ExportMode::BestEffort,
            &rejected_hashes,
        );
        let adrs = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "adrs")
            .unwrap();
        assert!(
            adrs.assertions
                .iter()
                .all(|assertion| !assertion.subject_id.starts_with("adr:recovered:"))
        );
        assert_eq!(bundle.drift_count, 0);
    }

    #[test]
    fn security_view_maps_findings_and_honors_support_curation() {
        // AC-0041/AC-0042 (T-0041/T-0042): explicit endpoint auth and IAM
        // wildcard support become mapped findings without bypassing R-INT-5.
        let endpoint = Node {
            id: "ep:admin".into(),
            label: "Endpoint".into(),
            props: serde_json::json!({
                "method": "GET",
                "path": "/admin",
                "authenticated": false,
                "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "admin-endpoint"),
            }),
        };
        let policy = node(
            "res:admin-policy",
            "Resource",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let bucket = node(
            "res:orders",
            "Resource",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let grant = Edge {
            src: policy.id.clone(),
            dst: bucket.id.clone(),
            label: "GRANTS".into(),
            props: serde_json::json!({
                "actions": ["s3:Get*"],
                "resource_scopes": ["arn:aws:s3:::orders/*"],
                "prov": prov(Tier::Agentic, ConfidenceTier::InferredWeak, "wildcard-grant"),
            }),
        };
        let grant_hash = provenance(&grant.props, "grant").content_hash;

        let bundle = compile_spec(
            &[endpoint.clone(), policy.clone(), bucket.clone()],
            std::slice::from_ref(&grant),
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let security = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "security-view")
            .unwrap();
        assert_eq!(bundle.security_count, 2);
        assert!(
            security
                .content
                .contains("No authentication check recovered: GET /admin")
        );
        assert!(security.content.contains("arn:aws:s3:::orders/*"));
        assert!(security.content.contains("US-0015 / AC-0041"));
        assert!(security.content.contains("US-0015 / AC-0042"));
        let grant_finding = security
            .assertions
            .iter()
            .find(|assertion| assertion.summary.contains("Over-broad IAM grant"))
            .unwrap();
        assert_eq!(
            grant_finding.provenance.confidence_tier,
            ConfidenceTier::InferredWeak
        );

        let curated = compile_spec(
            &[endpoint, policy, bucket],
            &[grant],
            &[],
            ExportMode::BestEffort,
            &BTreeSet::from([grant_hash]),
        );
        let curated_security = curated
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "security-view")
            .unwrap();
        assert_eq!(curated.security_count, 1);
        assert!(!curated_security.content.contains("s3:Get*"));
    }

    #[test]
    fn toolchain_artifact_states_what_the_system_is_built_with() {
        // #215 (AC-0096): the spec export gains a toolchain section fed by
        // the same Tool nodes the graph holds — settings cited, config file
        // named, presence-only code configs honestly marked.
        let react = Node {
            id: "tool:local/shop@react".into(),
            label: "Tool".into(),
            props: serde_json::json!({
                "name": "react",
                "display": "React",
                "category": "framework",
                "settings": { "requirement": "^19.0.0" },
                "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "react"),
            }),
        };
        let vite = Node {
            id: "tool:local/shop@vite.config.ts".into(),
            label: "Tool".into(),
            props: serde_json::json!({
                "name": "vite.config.ts",
                "display": "Vite config",
                "category": "bundler",
                "settings": {},
                "settings_behind_code": true,
                "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "vite"),
            }),
        };
        let manifest = Node {
            id: "file:local/shop@package.json".into(),
            label: "File".into(),
            props: serde_json::json!({
                "path": "package.json",
                "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "manifest"),
            }),
        };
        let defined_in = Edge {
            src: react.id.clone(),
            dst: manifest.id.clone(),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "defined"),
            }),
        };
        let bundle = compile_spec(
            &[react, vite, manifest],
            &[defined_in],
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let toolchain = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "toolchain")
            .unwrap();
        assert!(
            toolchain
                .content
                .contains("| React | framework | package.json |")
        );
        assert!(toolchain.content.contains(r#"requirement="^19.0.0""#));
        assert!(
            toolchain
                .content
                .contains("settings live in code — detected by presence only")
        );
        let assertion = toolchain
            .assertions
            .iter()
            .find(|assertion| assertion.subject_id == "tool:local/shop@react")
            .unwrap();
        assert_eq!(assertion.subject_kind, "Tool");
        assert_eq!(
            assertion.provenance.confidence_tier,
            ConfidenceTier::Confirmed
        );
        // An empty graph renders the honest empty row instead.
        let empty = compile_spec(&[], &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        assert!(
            empty
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "toolchain")
                .unwrap()
                .content
                .contains("No toolchain facts recovered")
        );
    }

    #[test]
    fn test_arrangements_count_static_references_and_unreferenced_endpoints() {
        // AC-0344 (T-0344).
        let node = |id: &str, label: &str, props: serde_json::Value, tier| {
            let mut props = props;
            props["prov"] = prov(Tier::Deterministic, tier, id);
            Node {
                id: id.into(),
                label: label.into(),
                props,
            }
        };
        let request = |dst: &str, statuses: serde_json::Value, tier| {
            let mut props = serde_json::json!({ "statuses": statuses, "keys": ["items"] });
            props["prov"] = prov(Tier::Deterministic, tier, dst);
            Edge {
                src: "test:local/app@tests/test_api.py#test_list".into(),
                dst: dst.into(),
                label: "REQUESTS".into(),
                props,
            }
        };
        let nodes = [
            node(
                "test:local/app@tests/test_api.py#test_list",
                "TestCase",
                serde_json::json!({ "name": "test_list" }),
                ConfidenceTier::Confirmed,
            ),
            node(
                "endpoint:list",
                "Endpoint",
                serde_json::json!({ "method": "GET", "path": "/api/items" }),
                ConfidenceTier::Confirmed,
            ),
            node(
                "endpoint:delete",
                "Endpoint",
                serde_json::json!({ "method": "DELETE", "path": "/api/items/<id>" }),
                ConfidenceTier::Confirmed,
            ),
            node(
                "gap:test-call:local/app@tests/test_api.py@10",
                "Gap",
                serde_json::json!({ "reason": "request URL is computed" }),
                ConfidenceTier::Gap,
            ),
        ];
        let edges = [
            request(
                "endpoint:list",
                serde_json::json!([200, 401]),
                ConfidenceTier::InferredStrong,
            ),
            request(
                "gap:test-call:local/app@tests/test_api.py@10",
                serde_json::Value::Null,
                ConfidenceTier::Gap,
            ),
        ];
        let artifact = |mode| {
            compile_spec(&nodes, &edges, &[], mode, &BTreeSet::new())
                .artifacts
                .into_iter()
                .find(|artifact| artifact.id == "test-arrangements")
                .unwrap()
        };
        let best = artifact(ExportMode::BestEffort);
        assert!(
            best.content
                .contains("it is not evidence that the test runs")
        );
        assert!(best.content.contains("| Recovered tests | 1 |"));
        assert!(best.content.contains("| Test → endpoint references | 1 |"));
        assert!(
            best.content
                .contains("| Unresolved test requests (see gap_register.md) | 1 |")
        );
        assert!(
            best.content
                .contains("| Endpoints referenced by a test | 1 of 2 |")
        );
        assert!(
            best.content
                .contains("| GET /api/items | tests/test_api.py#test_list | 200, 401 | items |")
        );
        assert!(best.content.contains("- DELETE /api/items/<id>"));
        let kinds: Vec<_> = best
            .assertions
            .iter()
            .map(|a| (a.subject_kind.as_str(), a.provenance.confidence_tier))
            .collect();
        assert_eq!(
            kinds,
            [
                ("REQUESTS", ConfidenceTier::InferredStrong),
                ("Endpoint", ConfidenceTier::Confirmed),
            ]
        );
        // A rejected inferred reference is suppressed, never upgraded.
        let rejected = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::from([provenance(&edges[0].props, "").content_hash]),
        )
        .artifacts
        .into_iter()
        .find(|artifact| artifact.id == "test-arrangements")
        .unwrap();
        assert!(
            rejected
                .content
                .contains("| Endpoints referenced by a test | 0 of 2 |")
        );
        assert!(rejected.content.contains("No test references recovered"));
    }

    #[test]
    fn flow_dossiers_name_static_test_references_per_participant() {
        // AC-0346 (T-0346).
        let mut handles = flow_hop(ConfidenceTier::Confirmed, "sym:a");
        handles.label = "HANDLES".into();
        handles.src = "ep:orders".into();
        handles.src_name = "POST /orders".into();
        let calls = flow_hop(ConfidenceTier::Confirmed, "sym:helper");
        let deeper = Hop {
            src: "sym:helper".into(),
            src_name: "sym:helper".into(),
            ..flow_hop(ConfidenceTier::Confirmed, "sym:deep")
        };
        let zero_hop = |id: &str, name: &str| Flow {
            trigger: id.into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: name.into(),
            hops: Vec::new(),
            status: FlowStatus::Verified,
            score: 1.0,
            depth_limited: false,
        };
        let flows = vec![
            Flow {
                trigger: "ep:orders".into(),
                trigger_kind: "Endpoint".into(),
                trigger_name: "POST /orders".into(),
                hops: vec![handles, calls, deeper],
                status: FlowStatus::Verified,
                score: 1.0,
                depth_limited: false,
            },
            // Zero-hop triggers are still participants a test may reference.
            zero_hop("ep:lonely", "GET /lonely"),
            zero_hop("ep:unreferenced", "GET /unreferenced"),
        ];
        let test = |n: usize| format!("test:local/shop@tests/test_orders.py#test_{n}");
        let mut nodes = vec![node(
            "ep:orders",
            "Endpoint",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        )];
        let mut edges = Vec::new();
        for n in 0..7 {
            nodes.push(node(
                &test(n),
                "TestCase",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ));
            let symbol = format!("sym:test_{n}");
            edges.push(edge(
                &test(n),
                &symbol,
                "IMPLEMENTED_BY",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ));
            // Seven tests call the helper directly; one also requests the endpoint.
            edges.push(edge(
                &symbol,
                "sym:helper",
                "CALLS",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ));
        }
        edges.push(edge(
            &test(0),
            "ep:orders",
            "REQUESTS",
            Tier::Deterministic,
            ConfidenceTier::InferredStrong,
        ));
        edges.push(edge(
            &test(2),
            "ep:lonely",
            "REQUESTS",
            Tier::Deterministic,
            ConfidenceTier::InferredStrong,
        ));
        // A test whose association to its function is only a Gap, and a
        // function no test is associated with, attribute no call to a test.
        nodes.push(node(
            &test(7),
            "TestCase",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ));
        edges.push(edge(
            &test(7),
            "sym:test_7",
            "IMPLEMENTED_BY",
            Tier::Deterministic,
            ConfidenceTier::Gap,
        ));
        for symbol in ["sym:test_7", "sym:unassociated"] {
            edges.push(edge(
                symbol,
                "sym:deep",
                "CALLS",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ));
        }
        // An unresolved request and a call from a non-test function prove nothing.
        edges.push(edge(
            &test(1),
            "sym:a",
            "REQUESTS",
            Tier::Deterministic,
            ConfidenceTier::Gap,
        ));
        edges.push(edge(
            "sym:other",
            "sym:deep",
            "CALLS",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ));
        let dossier = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        )
        .artifacts
        .into_iter()
        .find(|artifact| artifact.id == "flow-dossiers")
        .unwrap();
        let content = &dossier.content;
        assert!(content.contains("**Test references (static).** 2 of 4 participants"));
        assert!(content.contains("A reference is not an execution"));
        assert!(content.contains("| POST /orders | tests/test_orders.py#test_0 |"));
        assert!(content.contains("| sym:a | — no recovered test reference |"));
        assert!(content.contains("| sym:deep | — no recovered test reference |"));
        assert!(content.contains("tests/test_orders.py#test_4, and 2 more |"));
        assert!(!content.contains("test_7"));
        assert!(content.contains("**Test references (static).** 1 of 1 participants"));
        assert!(content.contains("| GET /lonely | tests/test_orders.py#test_2 |"));
        assert!(content.contains("**Test references (static).** 0 of 1 participants"));
        assert!(content.contains("| GET /unreferenced | — no recovered test reference |"));
        // Each reference cites every edge proving it: requests, and each
        // attributed call with its test's association.
        let cited = |kind: &str| {
            dossier
                .assertions
                .iter()
                .filter(|a| a.subject_kind == kind)
                .count()
        };
        assert_eq!(cited("REQUESTS"), 2);
        assert_eq!(cited("CALLS"), 7);
        assert_eq!(cited("IMPLEMENTED_BY"), 7);
        // Without recovered tests the dossier is unchanged.
        let plain = compile_spec(
            &nodes[..1],
            &[],
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        )
        .artifacts
        .into_iter()
        .find(|artifact| artifact.id == "flow-dossiers")
        .unwrap();
        assert!(!plain.content.contains("Test references"));
    }

    fn reason_gap(id: &str, reason: &str) -> Node {
        Node {
            id: id.into(),
            label: "Gap".into(),
            props: serde_json::json!({
                "reason": reason,
                "prov": prov(Tier::Deterministic, ConfidenceTier::Gap, id),
            }),
        }
    }

    fn gap_artifacts(bundle: &SpecBundle) -> (&SpecArtifact, serde_json::Value) {
        let register = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.file_name == "gap_register.md")
            .unwrap();
        let index = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.file_name == "gap_register.json")
            .unwrap();
        assert_eq!(index.format, "json");
        assert!(index.assertions.is_empty(), "instances are counted once");
        (register, serde_json::from_str(&index.content).unwrap())
    }

    #[test]
    fn gap_register_groups_caps_and_indexes_every_instance() {
        // AC-0210 (T-0210): a large register renders bounded, grouped prose
        // with counted omissions, and the JSON sidecar lists every instance.
        let mut nodes: Vec<Node> = (0..30)
            .map(|index| reason_gap(&format!("gap:a{index:03}"), "computed identity"))
            .collect();
        nodes.extend((0..8).map(|index| reason_gap(&format!("gap:b{index:03}"), "dynamic import")));
        nodes.push(reason_gap("gap:c000", "eval"));
        nodes.push(node(
            "sym:a",
            "Symbol",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ));
        let mut edges: Vec<Edge> = (0..7)
            .map(|index| {
                edge(
                    "sym:a",
                    &format!("sym:missing{index}"),
                    "CALLS",
                    Tier::Deterministic,
                    ConfidenceTier::Gap,
                )
            })
            .collect();
        edges.push(edge(
            "sym:a",
            "gap:a000",
            "DEPENDS_ON",
            Tier::Deterministic,
            ConfidenceTier::Gap,
        ));
        let compile = || {
            compile_spec(
                &nodes,
                &edges,
                &[],
                ExportMode::VerifiedOnly,
                &BTreeSet::new(),
            )
        };
        let bundle = compile();
        let (register, index) = gap_artifacts(&bundle);
        let total = register.assertions.len();
        assert_eq!(
            total,
            30 + 8 + 1 + 7 + 1,
            "every gap stays a structured assertion"
        );

        // Classes: largest first; the omission count plus the representatives
        // equals each class's size.
        let classes = index["classes"].as_array().unwrap();
        let summary: Vec<(&str, u64)> = classes
            .iter()
            .map(|class| {
                (
                    class["cause"].as_str().unwrap(),
                    class["instances"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("computed identity", 30),
                ("dynamic import", 8),
                ("unresolved CALLS edge", 7),
                ("eval", 1),
            ]
        );
        // The DEPENDS_ON edge into gap:a000 supports that node's finding, as
        // in the Gaps lane, so it is indexed as supporting, not a class.
        assert_eq!(
            index["supporting"],
            serde_json::json!(["edge:sym:a DEPENDS_ON gap:a000"])
        );
        let content = &register.content;
        assert!(content.contains(
            "| C-01 | computed identity | `spec.workbench.test` | T1 | unclassified (unresolved relationship) | interpretation | 30 |"
        ));
        // AC-0254 (#585): the register groups findings by omission class and
        // names the seam that would fill each.
        assert!(content.contains(
            "## By omission class\n\nWhy each finding is missing, and the seam that would fill it.\n\n\
             | Omission class | Filled by | Findings |\n|---|---|---|\n\
             | unresolved relationship | adapter | 7 |\n\
             | unclassified (unresolved relationship) | interpretation | 39 |\n"
        ));
        assert_eq!(
            index["by_omission"],
            serde_json::json!([
                {"class": "unresolved_relationship", "seam": "adapter", "findings": 7},
                {"class": "unresolved_relationship", "seam": "interpretation", "unclassified": true, "findings": 39},
            ])
        );
        assert_eq!(classes[2]["omission_class"], "unresolved_relationship");
        assert_eq!(classes[2]["seam"], "adapter");
        // Every Gap assertion carries its own omission; an unrecognized
        // reason says it is unclassified rather than guessing.
        assert!(
            register
                .assertions
                .iter()
                .all(|assertion| assertion.omission.is_some())
        );
        assert!(
            register
                .assertions
                .iter()
                .filter_map(|assertion| assertion.omission.as_ref())
                .any(|omission| omission.detail == "unclassified: computed identity")
        );
        assert!(content.contains("## C-01 — computed identity (30 instances)"));
        assert!(content.contains(
            "… 25 more instances in this class — listed in `gap_register.json` under `C-01`."
        ));
        assert!(content.contains(
            "… 3 more instances in this class — listed in `gap_register.json` under `C-02`."
        ));
        assert!(content.contains("… 2 more instances in this class"));
        assert!(content.contains("`gap:a004`") && !content.contains("`gap:a005`"));
        assert!(content.contains("Inline provenance is shown for the 16 representative instances above; the other 31 carry theirs"));

        // The sidecar indexes every register assertion exactly once.
        let mut indexed: Vec<&str> = classes
            .iter()
            .flat_map(|class| class["members"].as_array().unwrap())
            .chain(index["supporting"].as_array().unwrap())
            .map(|member| member.as_str().unwrap())
            .collect();
        indexed.sort_unstable();
        let mut ids: Vec<&str> = register.assertions.iter().map(|a| a.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(indexed, ids);
        assert_eq!(index["instances"], total);
        assert_eq!(index["findings"], bundle.gap_count);

        // Deterministic across compiles.
        assert_eq!(
            serde_json::to_string(&bundle).unwrap(),
            serde_json::to_string(&compile()).unwrap()
        );
    }

    #[test]
    fn gap_register_prose_stays_bounded_as_the_register_grows() {
        // AC-0210 (T-0210): past the class cap, remaining classes are one
        // counted line, and prose size does not track instance count.
        let register_for = |per_class: usize| {
            let nodes: Vec<Node> = (0..GAP_REGISTER_MAX_CLASSES + 10)
                .flat_map(|class| {
                    (0..per_class).map(move |index| {
                        reason_gap(
                            &format!("gap:{class:03}:{index:05}"),
                            &format!("cause {class:03}"),
                        )
                    })
                })
                .collect();
            let bundle = compile_spec(&nodes, &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
            let (register, index) = gap_artifacts(&bundle);
            assert_eq!(register.assertions.len(), nodes.len());
            assert_eq!(
                index["classes"].as_array().unwrap().len(),
                GAP_REGISTER_MAX_CLASSES + 10
            );
            register.content.clone()
        };
        let small = register_for(10);
        let large = register_for(200);
        assert!(small.contains("| … | 10 more classes, not detailed here — listed in `gap_register.json` | — | — | — | — | 100 |"));
        assert!(large.contains("| … | 10 more classes, not detailed here — listed in `gap_register.json` | — | — | — | — | 2000 |"));
        assert!(!large.contains("## C-51"));
        // 20x the instances, same shape: only the digits of counts differ.
        assert!(
            large.len() < small.len() + 1024,
            "{} vs {}",
            large.len(),
            small.len()
        );
    }

    #[test]
    fn gap_register_classes_match_the_gaps_lane() {
        // AC-0210 (T-0210): 13 unresolved calls each emit a Gap node plus
        // its supporting Gap CALLS edge; the Markdown groups them into one
        // 13-instance class, as the Gaps lane does, never 26 rows in two.
        let mut nodes: Vec<Node> = (0..13)
            .map(|index| reason_gap(&format!("gap:call{index:02}"), "unresolved call"))
            .collect();
        nodes.push(node(
            "sym:a",
            "Symbol",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ));
        let edges: Vec<Edge> = (0..13)
            .map(|index| {
                edge(
                    "sym:a",
                    &format!("gap:call{index:02}"),
                    "CALLS",
                    Tier::Deterministic,
                    ConfidenceTier::Gap,
                )
            })
            .collect();
        let bundle = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let (register, index) = gap_artifacts(&bundle);
        assert_eq!(
            register.assertions.len(),
            26,
            "every row stays an assertion"
        );
        let classes = index["classes"].as_array().unwrap();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0]["cause"], "unresolved call");
        assert_eq!(classes[0]["instances"], 13);
        assert_eq!(index["supporting"].as_array().unwrap().len(), 13);
        assert!(register.content.contains("13 findings in 1 cause classes"));
        assert!(!register.content.contains("unresolved CALLS edge"));
    }

    #[test]
    fn small_gap_register_stays_one_flat_table() {
        // AC-0210: at or below the Gaps lane's grouping threshold the register
        // keeps every row and its full provenance table.
        let nodes: Vec<Node> = (0..GAP_REGISTER_FLAT_LIMIT)
            .map(|index| reason_gap(&format!("gap:{index:02}"), "computed identity"))
            .collect();
        let bundle = compile_spec(&nodes, &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        let (register, index) = gap_artifacts(&bundle);
        assert!(
            register
                .content
                .starts_with("# Gap register\n\n| Subject | Reason |")
        );
        assert!(!register.content.contains("more instances"));
        for node in &nodes {
            assert_eq!(
                register.content.matches(&format!("`{}`", node.id)).count(),
                1
            );
        }
        assert_eq!(
            register
                .content
                .matches("| Gap: computed identity | Deterministic | Gap |")
                .count(),
            GAP_REGISTER_FLAT_LIMIT
        );
        assert_eq!(index["instances"], GAP_REGISTER_FLAT_LIMIT);
    }

    fn matrix_artifacts(bundle: &SpecBundle) -> (&SpecArtifact, serde_json::Value) {
        let matrix = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.file_name == "US-TM.md")
            .unwrap();
        let index = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.file_name == "US-TM.json")
            .unwrap();
        assert!(index.assertions.is_empty());
        (matrix, serde_json::from_str(&index.content).unwrap())
    }

    /// `big` sources REALIZE `cap:big`, 20 realize `cap:mid`, and `small`
    /// classes of two MAPS_TO links each.
    fn matrix_edges(big: usize, small: usize) -> Vec<Edge> {
        let confirmed = |src: String, dst: &str, label: &str| {
            edge(
                &src,
                dst,
                label,
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            )
        };
        let mut edges: Vec<Edge> = (0..big)
            .map(|index| confirmed(format!("flow:big:{index:05}"), "cap:big", "REALIZES"))
            .collect();
        edges.extend(
            (0..20).map(|index| confirmed(format!("flow:mid:{index:05}"), "cap:mid", "REALIZES")),
        );
        for class in 0..small {
            for index in 0..2 {
                edges.push(confirmed(
                    format!("sym:{class:03}:{index}"),
                    &format!("domain:{class:03}"),
                    "MAPS_TO",
                ));
            }
        }
        edges
    }

    #[test]
    fn traceability_matrix_groups_caps_and_indexes_every_link() {
        // AC-0221 (T-0221): past 200 links, US-TM.md groups by relation ×
        // target, caps classes and representatives with counted lines, and
        // US-TM.json indexes every link exactly once.
        let edges = matrix_edges(150, US_TM_MAX_CLASSES + 10);
        let links = 150 + 20 + 2 * (US_TM_MAX_CLASSES + 10);
        let bundle = compile_spec(&[], &edges, &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        let (matrix, index) = matrix_artifacts(&bundle);
        assert_eq!(matrix.assertions.len(), links);
        for expected in [
            "| M-01 | REALIZES | `cap:big` | 150 |",
            "| M-02 | REALIZES | `cap:mid` | 20 |",
            "| M-03 | MAPS_TO | `domain:000` | 2 |",
            "… 145 more links in this class — listed in `US-TM.json` under `M-01`.",
            "… 15 more links in this class — listed in `US-TM.json` under `M-02`.",
            "| … | 12 more classes, not detailed here — listed in `US-TM.json` | — | 24 |",
        ] {
            assert!(matrix.content.contains(expected), "missing {expected}");
        }
        // No class past the cap is detailed, and classes of two list both.
        assert!(!matrix.content.contains("## M-51"));
        assert!(
            !matrix
                .content
                .contains("more links in this class — listed in `US-TM.json` under `M-03`")
        );
        let representatives = 5 + 5 + 2 * (US_TM_MAX_CLASSES - 2);
        assert!(matrix.content.contains(&format!(
            "Inline provenance is shown for the {representatives} representative links above; the other {} carry",
            links - representatives
        )));

        assert_eq!(index["schema"], "cartograph.us-tm-index/v1");
        assert_eq!(index["links"], links);
        let classes = index["classes"].as_array().unwrap();
        assert_eq!(classes.len(), 2 + US_TM_MAX_CLASSES + 10);
        assert_eq!(classes[0]["id"], "M-01");
        assert_eq!(classes[0]["target"], "cap:big");
        let mut indexed: Vec<&str> = classes
            .iter()
            .flat_map(|class| class["members"].as_array().unwrap())
            .map(|member| member.as_str().unwrap())
            .collect();
        indexed.sort_unstable();
        let mut asserted: Vec<&str> = matrix
            .assertions
            .iter()
            .map(|assertion| assertion.id.as_str())
            .collect();
        asserted.sort_unstable();
        assert_eq!(indexed, asserted, "every link is indexed exactly once");

        let again = compile_spec(&[], &edges, &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        let (matrix_again, _) = matrix_artifacts(&again);
        assert_eq!(matrix.content, matrix_again.content);
        assert_eq!(
            serde_json::to_string(&bundle.artifacts).unwrap(),
            serde_json::to_string(&again.artifacts).unwrap()
        );
    }

    #[test]
    fn traceability_matrix_prose_stays_bounded_as_links_grow() {
        // AC-0221 (T-0221): prose size does not track link count.
        let content_for = |big: usize| {
            let edges = matrix_edges(big, US_TM_MAX_CLASSES + 10);
            let bundle = compile_spec(&[], &edges, &[], ExportMode::VerifiedOnly, &BTreeSet::new());
            let (matrix, index) = matrix_artifacts(&bundle);
            assert_eq!(matrix.assertions.len(), edges.len());
            assert_eq!(index["links"], edges.len());
            matrix.content.clone()
        };
        let small = content_for(300);
        let large = content_for(30_000);
        assert!(
            large.len() < small.len() + 64,
            "{} vs {}",
            large.len(),
            small.len()
        );
    }

    #[test]
    fn small_traceability_matrix_stays_one_flat_table() {
        // AC-0221 (T-0221): at or below the flat limit nothing is grouped.
        let edges = matrix_edges(10, 3);
        let bundle = compile_spec(&[], &edges, &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        let (matrix, index) = matrix_artifacts(&bundle);
        assert!(matrix.content.starts_with(
            "# Recovered US traceability matrix\n\n| Source | Relation | Target |\n|---|---|---|\n"
        ));
        assert!(!matrix.content.contains("| Class |"));
        assert!(!matrix.content.contains("more links"));
        for assertion in &matrix.assertions {
            assert!(matrix.content.contains(&assertion.provenance.content_hash));
        }
        assert_eq!(index["links"], 36);
        assert_eq!(index["classes"].as_array().unwrap().len(), 5);
    }

    // AC-0258 (T-0258): selected construction and ordinary compatibility.
    #[test]
    fn selected_producers_preserve_complete_artifacts_for_both_projection_modes() {
        let (nodes, edges, flows) = fixture();
        for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
            for rejected in [
                BTreeSet::new(),
                nodes
                    .iter()
                    .map(|n| provenance(&n.props, &n.id).content_hash)
                    .collect(),
            ] {
                let full = compile_spec(&nodes, &edges, &flows, mode, &rejected);
                assert_eq!(full.artifacts.len(), 32);
                for artifact in &full.artifacts {
                    let selected = compile_spec_selected(
                        &nodes,
                        &edges,
                        &flows,
                        mode,
                        &rejected,
                        &[&artifact.id],
                        SelectedCompileLimits {
                            max_bytes: usize::MAX,
                            adr: crate::AdrLimits {
                                max_rows: usize::MAX,
                                max_output_bytes: usize::MAX,
                            },
                        },
                    )
                    .unwrap();
                    assert_eq!(
                        selected,
                        vec![SelectedArtifactOutcome::Produced(artifact.clone())],
                        "{}",
                        artifact.id
                    );
                }
                let none = compile_spec_selected(
                    &nodes,
                    &edges,
                    &flows,
                    mode,
                    &rejected,
                    &[],
                    SelectedCompileLimits {
                        max_bytes: 0,
                        adr: crate::AdrLimits {
                            max_rows: 0,
                            max_output_bytes: 0,
                        },
                    },
                )
                .unwrap();
                assert!(none.is_empty());
            }
        }
    }

    // AC-0258 (T-0258): selected construction and ordinary compatibility.
    #[test]
    fn ordinary_missing_invalid_provenance_matches_original_bytes() {
        let (mut nodes, mut edges, mut flows) = fixture();
        for (i, node) in nodes.iter_mut().enumerate() {
            if i % 2 == 0 {
                node.props.as_object_mut().unwrap().remove("prov");
            } else {
                node.props["prov"] = serde_json::json!({"invalid":"stored provenance"});
            }
        }
        for (i, edge) in edges.iter_mut().enumerate() {
            if i % 2 == 0 {
                edge.props.as_object_mut().unwrap().remove("prov");
            } else {
                edge.props["prov"] = serde_json::json!({"invalid":"stored provenance"});
            }
        }
        flows[0].hops.clear();
        // Frozen against native #603 03d776d, incorporating existing method,
        // projection and identity seams; independently compared before removal.
        // The test-arrangements (#946) and journeys artifacts extend the bundle;
        // verified-only journeys state that they withhold InferredWeak candidates.
        // security.md states its coverage limit (#1820), which changes its bytes.
        // journey-candidate@2 (ADR-0084) changes only the printed rule version.
        for (mode, expected) in [
            (
                ExportMode::VerifiedOnly,
                "41fc16928d61831b0972dd1b8f27f75c7d8e570ca0ee2dd0650b57ee9b04b33f",
            ),
            (
                ExportMode::BestEffort,
                "9e0436fe375c8ea639476f64953b3dcb9db023020eff89270b224a3e2d366faf",
            ),
        ] {
            let full =
                graph_artifacts(compile_spec(&nodes, &edges, &flows, mode, &BTreeSet::new()));
            assert_eq!(
                blake3::hash(&serde_json::to_vec(&full).unwrap())
                    .to_hex()
                    .as_str(),
                expected
            );
            for artifact in full.artifacts {
                assert_eq!(
                    compile_spec_selected(
                        &nodes,
                        &edges,
                        &flows,
                        mode,
                        &BTreeSet::new(),
                        &[&artifact.id],
                        selected_limits(usize::MAX)
                    )
                    .unwrap(),
                    vec![SelectedArtifactOutcome::Produced(artifact)]
                );
            }
        }
    }

    // AC-0524 (T-0524): each Markdown register and its JSON index come from
    // one producer pass in ordinary compilation, with unchanged artifacts.
    #[test]
    fn paired_register_artifacts_match_single_artifact_compilation() {
        let (mut nodes, mut edges, flows) = fixture();
        let (t, gap) = (Tier::Deterministic, ConfidenceTier::Gap);
        for i in 0..crate::rules::RULE_EVIDENCE_FLAT_LIMIT + 5 {
            let id = format!("rule:{i:03}");
            nodes.push(node(&id, "BusinessRule", t, ConfidenceTier::Confirmed));
            nodes.push(node(&format!("gap:{i:03}"), "Gap", t, gap));
            edges.push(edge(&id, &format!("gap:{i:03}"), "GUARDS", t, gap));
        }
        let paired = [
            "us-tm",
            "us-tm-index",
            "gap-register",
            "gap-register-index",
            "rule-evidence",
            "rule-evidence-index",
        ];
        for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
            let full = compile_spec(&nodes, &edges, &flows, mode, &BTreeSet::new());
            for id in paired {
                let artifact = full.artifacts.iter().find(|a| a.id == id).unwrap();
                let selected = compile_spec_selected(
                    &nodes,
                    &edges,
                    &flows,
                    mode,
                    &BTreeSet::new(),
                    &[id],
                    selected_limits(usize::MAX),
                )
                .unwrap();
                assert_eq!(
                    selected,
                    vec![SelectedArtifactOutcome::Produced(artifact.clone())],
                    "{id}"
                );
            }
            let rules = full
                .artifacts
                .iter()
                .find(|a| a.id == "rule-evidence")
                .unwrap();
            assert!(
                rules.content.contains("observations from"),
                "grouped rule Markdown"
            );
        }
    }

    // AC-0525 (T-0525): recovered base-story text and provenance stay literal.
    #[test]
    fn base_stories_render_hostile_text_literally_within_exact_bounds() {
        let hostile = "Orders\n# Injected\n```\n<script>x</script> `tick` *b* ~~s~~ [l](u) | p";
        let mut capability = node(
            &format!("cap:{hostile}"),
            "Capability",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        capability.props["name"] = serde_json::json!(hostile);
        capability.props["prov"]["extractor_id"] = serde_json::json!("x<b>`|~~e~~");
        let nodes = vec![capability];
        let bundle = compile_spec(&nodes, &[], &[], ExportMode::BestEffort, &BTreeSet::new());
        let stories = bundle
            .artifacts
            .iter()
            .find(|a| a.id == "user-stories")
            .unwrap();
        for raw in [
            "<script>",
            "<b>",
            "```",
            "`tick`",
            "\n# Injected",
            "*b*",
            "~~",
            "[l](u)",
        ] {
            assert!(
                !stories.content.contains(raw),
                "{raw:?} in {}",
                stories.content
            );
        }
        assert!(stories.content.contains("&lt;script&gt;"));
        assert_eq!(
            stories
                .content
                .lines()
                .filter(|l| l.starts_with('#'))
                .count(),
            3,
            "title, story heading, assertions heading and nothing injected"
        );
        // Structured assertions keep exact source text and provenance.
        assert_eq!(stories.assertions[0].subject_id, nodes[0].id);
        assert_eq!(stories.assertions[0].provenance.extractor_id, "x<b>`|~~e~~");
        let exact = stories.content.len() + serde_json::to_vec(&stories.assertions).unwrap().len();
        for (max, produced) in [(exact, true), (exact - 1, false)] {
            let outcome = compile_spec_selected(
                &nodes,
                &[],
                &[],
                ExportMode::BestEffort,
                &BTreeSet::new(),
                &["user-stories"],
                selected_limits(max),
            )
            .unwrap();
            assert_eq!(
                matches!(&outcome[0], SelectedArtifactOutcome::Produced(a) if a == stories),
                produced,
                "{max}"
            );
        }
    }

    fn selected_limits(max_bytes: usize) -> SelectedCompileLimits {
        SelectedCompileLimits {
            max_bytes,
            adr: crate::AdrLimits {
                max_rows: usize::MAX,
                max_output_bytes: usize::MAX,
            },
        }
    }

    // AC-0258 (T-0258): selected construction and ordinary compatibility.
    #[test]
    fn selected_output_exact_boundaries_include_escaped_complete_assertion_arrays() {
        let (mut nodes, mut edges, mut flows) = fixture();
        for node in &mut nodes {
            node.props["name"] = serde_json::json!("quote\" | \n\r é\0".repeat(20));
        }
        edges[0].src.push_str("\"\n\0é");
        flows[0].hops[0].src_name = "é|\"\n\0".repeat(40);
        flows[0].hops[0].provenance.method = core_prov::ProducingMethod::Model {
            model: "quote\" | \n é\0".repeat(10),
        };
        for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
            let full = compile_spec(&nodes, &edges, &flows, mode, &BTreeSet::new());
            let mut total = 0;
            for artifact in &full.artifacts {
                let exact = artifact.content.len()
                    + serde_json::to_vec(&artifact.assertions).unwrap().len();
                total += exact;
                let selected = compile_spec_selected(
                    &nodes,
                    &edges,
                    &flows,
                    mode,
                    &BTreeSet::new(),
                    &[&artifact.id],
                    selected_limits(exact),
                )
                .unwrap();
                assert_eq!(
                    selected,
                    vec![SelectedArtifactOutcome::Produced(artifact.clone())],
                    "{}",
                    artifact.id
                );
                let refused = compile_spec_selected(
                    &nodes,
                    &edges,
                    &flows,
                    mode,
                    &BTreeSet::new(),
                    &[&artifact.id],
                    selected_limits(exact - 1),
                )
                .unwrap();
                assert_eq!(
                    refused,
                    vec![SelectedArtifactOutcome::Limited {
                        artifact_id: artifact.id.clone(),
                        limit: SelectedArtifactLimit::OutputBytes {
                            remaining: exact - 1
                        }
                    }]
                );
            }
            let mut ids: Vec<_> = full.artifacts.iter().rev().map(|a| a.id.as_str()).collect();
            ids.push("user-stories");
            ids.push("toolchain");
            let selected = compile_spec_selected(
                &nodes,
                &edges,
                &flows,
                mode,
                &BTreeSet::new(),
                &ids,
                selected_limits(total),
            )
            .unwrap();
            assert_eq!(
                selected,
                full.artifacts
                    .into_iter()
                    .map(SelectedArtifactOutcome::Produced)
                    .collect::<Vec<_>>()
            );
        }
    }

    // AC-0258 (T-0258): selected construction and ordinary compatibility.
    #[test]
    fn selection_order_duplicates_failures_and_unknown_ids_have_explicit_semantics() {
        let n = node(
            "cap:large",
            "Capability",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let mut large = n.clone();
        large.props["name"] = serde_json::json!("large".repeat(20_000));
        let full = compile_spec(
            &[large.clone()],
            &[],
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let matrix = full.artifacts.iter().find(|a| a.id == "us-tm").unwrap();
        let bytes = matrix.content.len() + 2;
        let selection = ["us-tm", "user-stories", "us-tm"];
        let result = compile_spec_selected(
            &[large.clone()],
            &[],
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
            &selection,
            selected_limits(bytes),
        )
        .unwrap();
        assert_eq!(
            result,
            vec![
                SelectedArtifactOutcome::Limited {
                    artifact_id: "user-stories".into(),
                    limit: SelectedArtifactLimit::OutputBytes { remaining: bytes }
                },
                SelectedArtifactOutcome::Produced(matrix.clone())
            ]
        );
        let reversed = compile_spec_selected(
            &[large],
            &[],
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
            &["user-stories", "us-tm"],
            selected_limits(bytes),
        )
        .unwrap();
        assert_eq!(result, reversed);
        let none = SelectedCompileLimits {
            max_bytes: 0,
            adr: crate::AdrLimits {
                max_rows: 0,
                max_output_bytes: 0,
            },
        };
        assert_eq!(
            compile_spec_selected(
                &[n],
                &[],
                &[],
                ExportMode::VerifiedOnly,
                &BTreeSet::new(),
                &["adrs", "unrecognized"],
                none
            ),
            Err(ArtifactSelectionError::UnknownArtifact { index: 1 })
        );
        assert!(
            compile_spec_selected(
                &[],
                &[],
                &[],
                ExportMode::VerifiedOnly,
                &BTreeSet::new(),
                &[],
                none
            )
            .unwrap()
            .is_empty()
        );
        let failed = compile_spec_selected(
            &[],
            &[],
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
            &["adrs", "user-stories"],
            none,
        )
        .unwrap();
        assert_eq!(
            failed,
            vec!["user-stories", "adrs"]
                .into_iter()
                .map(|id| SelectedArtifactOutcome::Limited {
                    artifact_id: id.into(),
                    limit: SelectedArtifactLimit::AdrDerivation(crate::AdrLimitExceeded {
                        limit: crate::AdrLimit::OutputBytes,
                        maximum: 0
                    })
                })
                .collect::<Vec<_>>()
        );
    }

    fn reorder_keys(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::Object(map) => {
                let fields: Vec<String> = map
                    .iter()
                    .rev()
                    .map(|(key, child)| {
                        format!(
                            "{}:{}",
                            serde_json::to_string(key).unwrap(),
                            reorder_keys(child)
                        )
                    })
                    .collect();
                format!("{{{}}}", fields.join(","))
            }
            serde_json::Value::Array(items) => {
                let items: Vec<String> = items.iter().map(reorder_keys).collect();
                format!("[{}]", items.join(","))
            }
            other => other.to_string(),
        }
    }

    #[test]
    fn artifacts_carry_deterministic_schema_and_content_identity() {
        let (nodes, edges, flows) = fixture();
        let compile = || {
            compile_spec(
                &nodes,
                &edges,
                &flows,
                ExportMode::BestEffort,
                &BTreeSet::new(),
            )
        };
        let first = compile();
        let second = compile();
        assert!(!first.artifacts.is_empty());
        let mut seen = BTreeSet::new();
        for (a, b) in first.artifacts.iter().zip(&second.artifacts) {
            assert_eq!(
                a.schema,
                format!("cartograph.spec/{}@{}", a.id, SPEC_ARTIFACT_SCHEMA_VERSION)
            );
            assert!(
                a.content_id.starts_with("spec-artifact-v1:"),
                "{}",
                a.content_id
            );
            assert!(a.verify_identity(), "{} verifies", a.file_name);
            assert_eq!(
                a.content_id, b.content_id,
                "{} is deterministic",
                a.file_name
            );
            assert!(
                seen.insert(a.content_id.clone()),
                "{} identity is distinct",
                a.file_name
            );
        }
    }

    #[test]
    fn artifact_identity_changes_with_content_assertions_or_schema() {
        let (nodes, edges, flows) = fixture();
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let original = bundle
            .artifacts
            .iter()
            .find(|artifact| !artifact.assertions.is_empty())
            .expect("an artifact with assertions")
            .clone();

        let mut content = original.clone();
        content.content.push('\n');
        let mut assertion = original.clone();
        assertion.assertions[0].provenance.content_hash.push('x');
        let mut schema = original.clone();
        schema.schema = format!(
            "cartograph.spec/{}@{}",
            schema.id,
            SPEC_ARTIFACT_SCHEMA_VERSION + 1
        );
        for (what, changed) in [
            ("content", content),
            ("assertion", assertion),
            ("schema", schema),
        ] {
            assert!(
                !changed.verify_identity(),
                "a changed {what} fails verification"
            );
            assert_ne!(
                changed.compute_content_id(),
                original.content_id,
                "{what} is hashed"
            );
        }
    }

    // AC-0258 (T-0258): selected construction and ordinary compatibility.
    #[test]
    fn streamed_artifact_identity_matches_original_typed_tuple_bytes() {
        let (nodes, edges, flows) = fixture();
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        for mut artifact in bundle.artifacts {
            artifact
                .content
                .push_str("\nUnicode é and escaped \"\\\u{0000}");
            if let Some(assertion) = artifact.assertions.first_mut() {
                assertion.provenance.method = core_prov::ProducingMethod::Human {
                    actor: "reviewer é\n\u{0000}".into(),
                };
            }
            let legacy_bytes = serde_json::to_vec(&(
                &artifact.schema,
                &artifact.id,
                &artifact.file_name,
                &artifact.title,
                &artifact.format,
                &artifact.content,
                &artifact.assertions,
            ))
            .unwrap();
            let mut hash = blake3::Hasher::new();
            hash.update(b"cartograph:spec-artifact:v1\0");
            hash.update(&legacy_bytes);
            assert_eq!(
                artifact.compute_content_id(),
                format!("spec-artifact-v1:{}", hash.finalize().to_hex())
            );
        }
    }

    #[test]
    fn artifact_identity_survives_json_key_reordering() {
        let (nodes, edges, flows) = fixture();
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        for artifact in &bundle.artifacts {
            let value = serde_json::to_value(artifact).unwrap();
            let reordered: SpecArtifact = serde_json::from_str(&reorder_keys(&value)).unwrap();
            assert_eq!(&reordered, artifact);
            assert!(
                reordered.verify_identity(),
                "{} key order is irrelevant",
                artifact.file_name
            );
        }
    }

    #[test]
    fn exports_show_producing_method_beside_tier_and_confidence() {
        // AC-0244 (T-0244): exported assertions carry a Method column; facts
        // without a recorded method say so explicitly.
        let (nodes, edges, flows) = fixture();
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let table = bundle
            .artifacts
            .iter()
            .find(|artifact| {
                artifact
                    .content
                    .contains("## Assertions and inline provenance")
            })
            .expect("an artifact with inline provenance");
        assert!(
            table
                .content
                .contains("| Assertion | Tier | Confidence | Method | Evidence |")
        );
        assert!(table.content.contains("| not recorded |"));
    }
    #[test]
    fn flat_gap_register_names_each_gap_class_and_filling_seam() {
        // AC-0254 (#585): every Gap row names its omission class and seam.
        let (nodes, edges, flows) = fixture();
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let (register, _) = gap_artifacts(&bundle);
        assert!(!register.assertions.is_empty());
        assert!(register.assertions.len() <= GAP_REGISTER_FLAT_LIMIT);
        assert!(
            register
                .content
                .contains("| Subject | Reason | Omission class | Filled by |")
        );
        assert!(register.content.contains("## By omission class"));
        for assertion in &register.assertions {
            let omission = assertion
                .omission
                .as_ref()
                .expect("a Gap carries its omission");
            assert!(
                register.content.contains(&format!(
                    "| {} | {} |\n",
                    omission.class_label(),
                    omission.seam.label()
                )),
                "{} row names its class",
                assertion.id
            );
        }
        // Non-gap assertions never carry an omission.
        assert!(
            bundle
                .artifacts
                .iter()
                .flat_map(|artifact| &artifact.assertions)
                .filter(|assertion| assertion.provenance.confidence_tier != ConfidenceTier::Gap)
                .all(|assertion| assertion.omission.is_none())
        );
    }

    fn review_gap_prov() -> serde_json::Value {
        serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Gap,
                vec![],
                "review",
                b"gap",
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn gap_flow_hop_keeps_its_originating_explicit_omission() {
        // #603 review (Codex reproduction): a hop restating a Gap edge keeps
        // that edge's exact omission instead of reclassifying its reason.
        let prov = review_gap_prov();
        let explicit = serde_json::json!({"class": "product_gap", "seam": "requirement", "detail": "no refund rule specified"});
        let nodes = vec![
            Node {
                id: "ep".into(),
                label: "Endpoint".into(),
                props: serde_json::json!({"prov": prov}),
            },
            Node {
                id: "gap".into(),
                label: "Gap".into(),
                props: serde_json::json!({"prov": prov, "reason": "unresolved dependency", "omission": explicit}),
            },
        ];
        let edges = vec![Edge {
            src: "ep".into(),
            dst: "gap".into(),
            label: "HANDLES".into(),
            props: serde_json::json!({"prov": prov, "reason": "unresolved dependency", "omission": explicit}),
        }];
        let flows = flowtracer::trace(&nodes, &edges);
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let (register, _) = gap_artifacts(&bundle);
        let edge = register
            .assertions
            .iter()
            .find(|a| a.subject_kind == "HANDLES")
            .unwrap();
        let hop = register
            .assertions
            .iter()
            .find(|a| a.subject_kind == "FlowHop")
            .unwrap();
        let expected = edge.omission.as_ref().unwrap();
        assert_eq!(
            (expected.class, expected.seam),
            (OmissionClass::ProductGap, FillSeam::Requirement)
        );
        assert_eq!(hop.omission.as_ref(), Some(expected));
    }

    #[test]
    fn interface_dispatch_gap_groups_under_its_exclusion_in_the_register() {
        // AC-0285 (#711 review): the persisted Gap, not only the semantic
        // overlay, groups under the dynamic dispatch exclusion.
        let prov = review_gap_prov();
        let nodes = vec![
            Node {
                id: "sym".into(),
                label: "Symbol".into(),
                props: serde_json::json!({"prov": prov}),
            },
            Node {
                id: "gap".into(),
                label: "Gap".into(),
                props: serde_json::json!({"prov": prov, "callee": "IChannel.listen", "reason": "unresolved call target"}),
            },
        ];
        let edges = vec![Edge {
            src: "sym".into(),
            dst: "gap".into(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov": prov}),
        }];
        let flows = flowtracer::trace(&nodes, &edges);
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let (register, _) = gap_artifacts(&bundle);
        let gap = register
            .assertions
            .iter()
            .find(|a| a.subject_kind == "Gap")
            .unwrap();
        assert_eq!(
            gap.omission.as_ref().unwrap().detail,
            core_graph::omission::DYNAMIC_DISPATCH
        );
    }

    #[test]
    fn reversed_subscribes_hop_keeps_its_source_edge_omission() {
        // #603 review (Codex reproduction): SUBSCRIBES is stored consumer →
        // channel but traced channel → consumer; the hop still finds its edge.
        let confirmed = serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![],
                "review",
                b"confirmed",
            )
            .unwrap(),
        )
        .unwrap();
        let explicit = serde_json::json!({"class": "product_gap", "seam": "requirement", "detail": "consumer contract missing"});
        let nodes = vec![
            Node {
                id: "channel".into(),
                label: "Channel".into(),
                props: serde_json::json!({"prov": confirmed}),
            },
            Node {
                id: "consumer".into(),
                label: "Function".into(),
                props: serde_json::json!({"prov": confirmed}),
            },
        ];
        let edges = vec![Edge {
            src: "consumer".into(),
            dst: "channel".into(),
            label: "SUBSCRIBES".into(),
            props: serde_json::json!({"prov": review_gap_prov(), "reason": "unresolved dependency", "omission": explicit}),
        }];
        let flows = flowtracer::trace(&nodes, &edges);
        assert!(
            flows
                .iter()
                .flat_map(|flow| &flow.hops)
                .any(|hop| hop.src == "channel" && hop.dst == "consumer")
        );
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let (register, _) = gap_artifacts(&bundle);
        let edge = register
            .assertions
            .iter()
            .find(|a| a.subject_kind == "SUBSCRIBES")
            .unwrap();
        let hop = register
            .assertions
            .iter()
            .find(|a| a.subject_kind == "FlowHop")
            .unwrap();
        let expected = edge.omission.as_ref().unwrap();
        assert_eq!(
            (expected.class, expected.seam),
            (OmissionClass::ProductGap, FillSeam::Requirement)
        );
        assert_eq!(hop.omission.as_ref(), Some(expected));
    }

    #[test]
    fn unclassified_omission_is_visibly_unclassified_in_the_register() {
        // #603 review (Codex reproduction): the fallback is never shown as
        // an established class.
        let nodes = vec![Node {
            id: "gap".into(),
            label: "Gap".into(),
            props: serde_json::json!({"prov": review_gap_prov(), "reason": "something entirely new"}),
        }];
        let bundle = compile_spec(&nodes, &[], &[], ExportMode::BestEffort, &BTreeSet::new());
        let (register, index) = gap_artifacts(&bundle);
        assert_eq!(
            register.assertions[0].omission.as_ref().unwrap().detail,
            "unclassified: something entirely new"
        );
        assert!(
            register
                .content
                .contains("| unclassified (unresolved relationship) | interpretation |")
        );
        assert_eq!(index["by_omission"][0]["unclassified"], true);
    }

    #[test]
    fn count_interpretation_backlog_separates_established_interpretation_from_system_gaps() {
        // AC-0359 (T-0359, #994): only established FillSeam::Interpretation
        // omissions enter the interpretation backlog; adapter omissions,
        // unclassified fallbacks, and supporting edges folded into a gap node
        // do not.
        let nodes = vec![
            reason_gap(
                "gap:interp",
                "The caller's interpretation of this local return or throw is unknown.",
            ),
            reason_gap("gap:adapter", "unresolved call target"),
            reason_gap("gap:unclassified", "something entirely new"),
            node(
                "sym:a",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:b",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
        ];
        let mut folded_edge = edge(
            "sym:a",
            "gap:interp",
            "DEPENDS_ON",
            Tier::Deterministic,
            ConfidenceTier::Gap,
        );
        folded_edge.props["reason"] = serde_json::json!(
            "The caller's interpretation of this local return or throw is unknown."
        );
        let standalone_adapter_edge = edge(
            "sym:a",
            "sym:b",
            "CALLS",
            Tier::Deterministic,
            ConfidenceTier::Gap,
        );
        let mut standalone_interp_edge = edge(
            "sym:a",
            "sym:b",
            "ROUTES",
            Tier::Deterministic,
            ConfidenceTier::Gap,
        );
        standalone_interp_edge.props["reason"] =
            serde_json::json!("Source-rule interpretation remains incomplete.");

        let base_edges = [folded_edge.clone(), standalone_adapter_edge.clone()];
        let total = count_gap_findings(&nodes, &base_edges);
        let backlog = count_interpretation_backlog(&nodes, &base_edges);
        assert!(backlog <= total);
        assert_eq!(total, 4);
        assert_eq!(backlog, 1);

        let all_edges = [folded_edge, standalone_adapter_edge, standalone_interp_edge];
        let total_with_edge_interp = count_gap_findings(&nodes, &all_edges);
        let backlog_with_edge_interp = count_interpretation_backlog(&nodes, &all_edges);
        assert!(backlog_with_edge_interp <= total_with_edge_interp);
        assert_eq!(total_with_edge_interp, 5);
        assert_eq!(backlog_with_edge_interp, 2);
    }

    // AC-0338 (#833): a replayed migration history adds `## Schema drift`
    // to drift_register.md: what was compared, each finding sorted, and the
    // tables it could not check; an empty history still reads as checked.
    #[test]
    fn drift_register_renders_schema_drift_section() {
        let mut history = node(
            "migrations:local/shop@migrations",
            "MigrationHistory",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        for (key, value) in [
            ("location", serde_json::json!("migrations")),
            ("head", serde_json::json!("d5")),
            ("revisions", serde_json::json!(5)),
            ("modeled_tables_compared", serde_json::json!(2)),
            ("modeled_columns_compared", serde_json::json!(6)),
        ] {
            history.props[key] = value;
        }
        let drift = |key: &str, kind: &str, modeled: &str, migrated: &str| {
            let mut n = node(
                &format!("drift:schema:local/shop@{key}:{kind}"),
                "Drift",
                Tier::Deterministic,
                ConfidenceTier::InferredStrong,
            );
            for (k, v) in [
                ("kind", "schema-drift"),
                ("drift_kind", kind),
                ("key", key),
                ("modeled", modeled),
                ("migrated", migrated),
            ] {
                n.props[k] = serde_json::json!(v);
            }
            n
        };
        let mut gap = node(
            "gap:alembic-table:local/shop@job",
            "Gap",
            Tier::Deterministic,
            ConfidenceTier::Gap,
        );
        gap.props["kind"] = serde_json::json!("migration-gap");
        gap.props["table"] = serde_json::json!("job");
        gap.props["reason"] =
            serde_json::json!("schema drift not checked for `job`: raw SQL reshapes it");
        let register = |nodes: &[Node]| {
            compile_spec(nodes, &[], &[], ExportMode::BestEffort, &BTreeSet::new())
                .artifacts
                .into_iter()
                .find(|a| a.id == "drift-register")
                .unwrap()
                .content
        };
        let empty = register(std::slice::from_ref(&history));
        assert!(empty.contains("\n## Schema drift\n\nMigrations in `migrations` replayed to head `d5` (5 revisions): 2 modeled tables and 6 columns compared.\n\n| Finding | Table / column | Modeled | Migrated | Confidence |\n|---|---|---|---|---|\n| — | No schema drift recovered | — | — | — |\n"));
        assert!(empty.contains("| — | No ADR/code conflicts recovered |"));
        let content = register(&[
            history,
            drift(
                "users.note",
                "modeled-not-migrated",
                "app/models.py bytes 1..2",
                "absent at head d5",
            ),
            drift(
                "audit",
                "migrated-not-modeled",
                "no model maps it",
                "migrations/versions/e6.py bytes 3..4",
            ),
            gap,
        ]);
        let audit = content
            .find("| migrated-not-modeled | `audit` | no model maps it | migrations/versions/e6.py bytes 3..4 | InferredStrong |")
            .unwrap();
        let note = content
            .find("| modeled-not-migrated | `users.note` | app/models.py bytes 1..2 | absent at head d5 | InferredStrong |")
            .unwrap();
        let unchecked = content
            .find("| gap | `job` | schema drift not checked for `job`: raw SQL reshapes it | — | Gap |")
            .unwrap();
        assert!(audit < note && note < unchecked);
        // Schema drift is not listed as an ADR conflict.
        assert!(content.contains("| — | No ADR/code conflicts recovered |"));
    }
}
