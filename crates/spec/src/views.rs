//! Structured diagram projections bound to official artifact content identities
//! (US-0036, AC-0246, AC-0247).

use crate::{
    ExportMode, SpecArtifact, SpecBundle, TOPOLOGY_EDGE_LABELS,
    workbench::{DATA_EDGE_LABELS, filter_edges, filter_nodes, selected},
};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Provenance};
use flowtracer::{Flow, Hop};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Per-view node cap applied to compiled diagram projections so IPC payloads
/// and Cytoscape rendering remain bounded on large repositories (#488, #591).
pub const SPEC_VIEW_MAX_NODES: usize = 1500;

/// Per-view edge cap applied to compiled diagram projections (#488, #591).
pub const SPEC_VIEW_MAX_EDGES: usize = 3000;

/// Flow-count cap applied to the `flow-dossiers` diagram projection (#488, #591).
pub const SPEC_VIEW_MAX_FLOWS: usize = 200;

/// Observed deployment or runtime backing attached to an infrastructure
/// `Resource` node (US-0036, AC-0246, AC-0247).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentRole {
    /// Resource node id.
    pub resource_id: String,
    /// Extractor or state source (e.g. `t1.terraform-state`, `t1.pulumi-deployment`, `state.json`).
    pub source: String,
    /// Validated observation provenance when `observed_prov` is recorded on the resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Code-layer `Channel` node ids backed by this deployed resource via `BACKS`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backed_channels: Vec<String>,
}

/// One structured diagram view compiled from the same R-INT-5 and curation
/// projection as its official `SpecArtifact` (US-0036, AC-0246, AC-0247).
#[derive(Debug, Clone, Serialize)]
pub struct SpecDiagramView {
    /// Official artifact id (`topology`, `flow-dossiers`, or `data-model`).
    pub artifact_id: String,
    /// Versioned schema id matching the exported `SpecArtifact.schema` (AC-0242, AC-0246).
    pub schema: String,
    /// Content identity matching the exported `SpecArtifact.content_id` (AC-0242, AC-0246).
    pub content_id: String,
    /// Export mode (`verified-only` or `best-effort`) used to project this view.
    pub mode: ExportMode,
    /// Structured graph nodes in this view projection, carrying provenance and props.
    pub nodes: Vec<Node>,
    /// Total node count in this view projection before `SPEC_VIEW_MAX_NODES` truncation.
    pub nodes_total: usize,
    /// Structured graph edges in this view projection, carrying provenance and props.
    pub edges: Vec<Edge>,
    /// Total edge count in this view projection before node/edge cap truncation.
    pub edges_total: usize,
    /// Projected flows with R-INT-4/R-INT-5 hop projection applied (populated for `flow-dossiers`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flows: Vec<Flow>,
    /// Total flow count in this view projection before `SPEC_VIEW_MAX_FLOWS` truncation.
    pub flows_total: usize,
    /// Deployment roles keyed by resource node id (populated when T1 state/deployment facts exist).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deployments: Vec<DeploymentRole>,
    /// Whether nodes, edges, or flows were truncated to fit the per-view caps.
    pub truncated: bool,
}

impl SpecDiagramView {
    /// Verify that this structured view is bound to the exact schema and
    /// content identity of `artifact`, and that `artifact` itself is valid.
    pub fn matches_artifact(&self, artifact: &SpecArtifact) -> bool {
        self.artifact_id == artifact.id
            && self.schema == artifact.schema
            && self.content_id == artifact.content_id
            && artifact.verify_identity()
    }
}

/// Compiled structured diagram views for topology/deployment, flows, and data model,
/// each bound to its exported `SpecArtifact` schema and content identity (US-0036).
#[derive(Debug, Clone, Serialize)]
pub struct SpecViewBundle {
    /// R-INT-5 projection applied consistently to all views.
    pub mode: ExportMode,
    /// Resource topology and deployment view (`topology.md`).
    pub topology: SpecDiagramView,
    /// Flow and infrastructure roles view (`flow_dossiers.md`).
    pub flows: SpecDiagramView,
    /// Data model and access relations view (`data_model.md`).
    pub data_model: SpecDiagramView,
    /// The C4 model `architecture.md` draws (#2049): the app's
    /// Architecture view, levels 1 to 3 with the facts under each box.
    pub architecture: ArchitectureView,
}

/// The C4 model of `architecture.md`, for the app's Architecture view
/// (#2049). Element ids are `e{n}` and component ids `e{n}c{m}`, the same
/// ids `architecture.dsl` uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchitectureView {
    /// Always `architecture`.
    pub artifact_id: String,
    /// Schema of the `architecture.md` artifact this view was compiled beside.
    pub schema: String,
    /// Content identity of that artifact.
    pub content_id: String,
    /// R-INT-5 projection applied.
    pub mode: ExportMode,
    /// The software system's name.
    pub system: String,
    /// People, containers, data stores and external systems.
    pub elements: Vec<ArchitectureElement>,
    /// Relationships among `elements`.
    pub relations: Vec<ArchitectureRelation>,
    /// Per container, its components and their relationships.
    pub components: Vec<ArchitectureComponents>,
    /// Whether subsystem areas were supplied, so components could be drawn.
    pub areas_supplied: bool,
}

/// One C4 element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchitectureElement {
    /// `e{n}`, or `e{n}c{m}` for a component of container `e{n}`.
    pub id: String,
    /// `person`, `container`, `store`, `external` or `component`.
    pub kind: String,
    /// Display name.
    pub name: String,
    /// Technology, empty when none was recovered.
    pub technology: String,
    /// What it is, from the facts.
    pub description: String,
    /// The lowest confidence of the facts it rests on.
    pub confidence: ConfidenceTier,
    /// The facts it rests on, at most twelve.
    pub facts: Vec<ArchitectureFact>,
    /// How many facts it rests on.
    pub facts_total: usize,
}

/// One C4 relationship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchitectureRelation {
    /// Source element id.
    pub from: String,
    /// Target element id.
    pub to: String,
    /// The relationship as `architecture.md` labels it.
    pub label: String,
    /// Protocol or technology, empty when none.
    pub technology: String,
    /// The lowest confidence of the facts it rests on.
    pub confidence: ConfidenceTier,
    /// The facts it rests on, at most twelve.
    pub facts: Vec<ArchitectureFact>,
    /// How many facts it rests on.
    pub facts_total: usize,
}

/// One fact an element or relationship rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchitectureFact {
    /// Node id, or the `src LABEL dst` identity of an edge.
    pub id: String,
    /// Node label or edge label.
    pub kind: String,
    /// A short readable name.
    pub label: String,
    /// Tier, confidence and evidence spans.
    pub provenance: Provenance,
}

/// The components of one container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchitectureComponents {
    /// The container's element id.
    pub container: String,
    /// Its components, largest first.
    pub elements: Vec<ArchitectureElement>,
    /// Relationships among them and with the model's elements.
    pub relations: Vec<ArchitectureRelation>,
    /// Files the container owns that are in no area.
    pub unassigned_files: usize,
}

fn deployment_role_for(resource: &Node, edges: &[Edge]) -> Option<DeploymentRole> {
    let backed_channels: Vec<String> = edges
        .iter()
        .filter(|edge| edge.label == "BACKS" && edge.src == resource.id)
        .map(|edge| edge.dst.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let observed_prov =
        serde_json::from_value::<Provenance>(resource.props["observed_prov"].clone())
            .ok()
            .filter(|prov| prov.validate().is_ok());
    let has_observed = resource.props.get("observed").is_some()
        || observed_prov.is_some()
        || resource.props["resolved_by"].as_str().is_some()
        || !backed_channels.is_empty();
    if !has_observed {
        return None;
    }
    let fallback_prov = if observed_prov.is_none() {
        edges
            .iter()
            .find(|edge| edge.label == "BACKS" && edge.src == resource.id)
            .and_then(|edge| {
                serde_json::from_value::<Provenance>(crate::stored_prov(&edge.props).into_owned())
                    .ok()
                    .filter(|prov| prov.validate().is_ok())
            })
    } else {
        None
    };
    let prov = observed_prov.or(fallback_prov);
    let source = prov
        .as_ref()
        .map(|item| item.extractor_id.clone())
        .or_else(|| resource.props["resolved_by"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "t1.deployment".into());
    Some(DeploymentRole {
        resource_id: resource.id.clone(),
        source,
        provenance: prov,
        backed_channels,
    })
}

fn participant_label(id: &str, hop_confidence: Option<&str>) -> String {
    if hop_confidence == Some("Gap") || id.starts_with("gap:") {
        return "Gap".into();
    }
    match id.split(':').next().unwrap_or("") {
        "ep" => "Endpoint".into(),
        "sym" => "Symbol".into(),
        "chan" | "channel" => "Channel".into(),
        "res" => "Resource".into(),
        "component" => "Component".into(),
        "screen" => "Screen".into(),
        "extctx" => "ExtensionContext".into(),
        "extcmd" => "ExtensionCommand".into(),
        "file" => "File".into(),
        "data" => "DataEntity".into(),
        _ => "Unknown".into(),
    }
}

fn synthetic_participant_node(id: &str, name: &str, hop: &Hop) -> Node {
    let is_gap = hop.confidence == "Gap"
        || hop.provenance.confidence_tier == ConfidenceTier::Gap
        || id.starts_with("gap:");
    let label = participant_label(id, Some(hop.confidence.as_str()));
    let mut props = serde_json::json!({
        "name": name,
        "prov": hop.provenance,
    });
    if is_gap && let Some(reason) = &hop.gap_reason {
        props["reason"] = serde_json::Value::String(reason.clone());
    }
    Node {
        id: id.into(),
        label,
        props,
    }
}

fn bounded_diagram_view(
    artifact: &SpecArtifact,
    mode: ExportMode,
    mut nodes: Vec<Node>,
    mut edges: Vec<Edge>,
    mut flows: Vec<Flow>,
    include_deployments: bool,
) -> SpecDiagramView {
    let nodes_total = nodes.len();
    let edges_total = edges.len();
    let flows_total = flows.len();

    nodes.truncate(SPEC_VIEW_MAX_NODES);
    let retained_node_ids: BTreeSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    let deployments = if include_deployments {
        nodes
            .iter()
            .filter(|node| node.label == "Resource")
            .filter_map(|node| deployment_role_for(node, &edges))
            .collect()
    } else {
        Vec::new()
    };
    edges.retain(|edge| {
        retained_node_ids.contains(edge.src.as_str())
            && retained_node_ids.contains(edge.dst.as_str())
    });
    edges.truncate(SPEC_VIEW_MAX_EDGES);
    flows.truncate(SPEC_VIEW_MAX_FLOWS);

    let truncated =
        nodes.len() < nodes_total || edges.len() < edges_total || flows.len() < flows_total;
    // A view hands its facts to the Workbench whole, as they were stored: a
    // compacted `prov` or rule payload is parsed back to its object (#2215).
    for node in &mut nodes {
        crate::workbench::restore_stored(&node.label, &mut node.props);
    }
    for edge in &mut edges {
        crate::workbench::restore_stored(&edge.label, &mut edge.props);
    }

    SpecDiagramView {
        artifact_id: artifact.id.clone(),
        schema: artifact.schema.clone(),
        content_id: artifact.content_id.clone(),
        mode,
        nodes,
        nodes_total,
        edges,
        edges_total,
        flows,
        flows_total,
        deployments,
        truncated,
    }
}

// Shared projection keeps overview and selected-flow provenance semantics identical.
fn flow_diagram_view(
    artifact: &SpecArtifact,
    mode: ExportMode,
    by_id: &BTreeMap<&str, &Node>,
    filtered_edges: &[&Edge],
    flows: &[Flow],
) -> SpecDiagramView {
    // 2. Flow + infrastructure roles view: all projected flows, their hop
    // participants, and any infrastructure Resource nodes + BACKS/TRIGGERS/ROUTES
    // edges that back or route participants in those flows.
    // Participants come only from the flows the view keeps, in flow order, so
    // a later node/edge cap drops whole trailing flows' context rather than
    // arbitrary participants of retained flows.
    let retained_flows = &flows[..flows.len().min(SPEC_VIEW_MAX_FLOWS)];
    let mut flow_nodes: Vec<Node> = Vec::new();
    let mut flow_node_ids: BTreeSet<String> = BTreeSet::new();
    let mut flow_edges: Vec<Edge> = Vec::new();
    let mut flow_edge_keys: BTreeSet<(String, String, String)> = BTreeSet::new();
    for flow in retained_flows {
        if let Some(trigger_node) = by_id.get(flow.trigger.as_str())
            && flow_node_ids.insert(flow.trigger.clone())
        {
            flow_nodes.push((*trigger_node).clone());
        }
        for hop in &flow.hops {
            if flow_node_ids.insert(hop.src.clone()) {
                flow_nodes.push(
                    by_id
                        .get(hop.src.as_str())
                        .map(|node| (*node).clone())
                        .unwrap_or_else(|| Node {
                            id: hop.src.clone(),
                            label: participant_label(&hop.src, None),
                            props: serde_json::json!({
                                "name": hop.src_name,
                                "prov": hop.provenance,
                            }),
                        }),
                );
            }
            if flow_node_ids.insert(hop.dst.clone()) {
                flow_nodes.push(
                    by_id
                        .get(hop.dst.as_str())
                        .map(|node| (*node).clone())
                        .unwrap_or_else(|| {
                            synthetic_participant_node(&hop.dst, &hop.dst_name, hop)
                        }),
                );
            }
            if flow_edge_keys.insert((hop.src.clone(), hop.dst.clone(), hop.label.clone())) {
                flow_edges.push(Edge {
                    src: hop.src.clone(),
                    dst: hop.dst.clone(),
                    label: hop.label.clone(),
                    props: serde_json::json!({
                        "prov": hop.provenance,
                        "gap_reason": hop.gap_reason,
                        "attempted_tiers": hop.attempted_tiers,
                    }),
                });
            }
        }
    }
    let participant_ids = flow_node_ids.clone();
    for edge in filtered_edges {
        if !TOPOLOGY_EDGE_LABELS.contains(&edge.label.as_str()) {
            continue;
        }
        let src_is_res = by_id
            .get(edge.src.as_str())
            .is_some_and(|node| node.label == "Resource");
        let dst_is_res = by_id
            .get(edge.dst.as_str())
            .is_some_and(|node| node.label == "Resource");
        let touches_flow =
            participant_ids.contains(&edge.src) || participant_ids.contains(&edge.dst);
        if touches_flow && (src_is_res || dst_is_res) {
            for id in [&edge.src, &edge.dst] {
                if let Some(node) = by_id.get(id.as_str())
                    && flow_node_ids.insert(id.clone())
                {
                    flow_nodes.push((*node).clone());
                }
            }
            if flow_node_ids.contains(&edge.src)
                && flow_node_ids.contains(&edge.dst)
                && flow_edge_keys.insert((edge.src.clone(), edge.dst.clone(), edge.label.clone()))
            {
                flow_edges.push((*edge).clone());
            }
        }
    }

    bounded_diagram_view(artifact, mode, flow_nodes, flow_edges, flows.to_vec(), true)
}

/// Compile one selected flow within existing diagram bounds, bound to the
/// complete official dossier identity. Selection precedes participant limits.
pub fn compile_selected_flow_view(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
    trigger: &str,
    expected_content_id: &str,
) -> Result<SpecDiagramView, String> {
    let projection = selected::project(nodes, edges, flows, mode, rejected_hashes, None)
        .map_err(|error| format!("flow projection failed: {error:?}"))?;
    let selected_flow = projection
        .flows
        .iter()
        .find(|flow| flow.trigger == trigger)
        .ok_or_else(|| "selected flow is no longer available; refresh the flow view".to_string())?;
    let filtered_nodes = filter_nodes(&projection.nodes, mode, rejected_hashes);
    let filtered_edges = filter_edges(&projection.edges, mode, rejected_hashes);
    let artifact = selected::flow_artifact_from_filtered(
        &filtered_nodes,
        &filtered_edges,
        &projection.flows,
        mode,
        rejected_hashes,
    );
    if artifact.content_id != expected_content_id {
        return Err("flow dossier changed since this preview; refresh the flow view".into());
    }
    let by_id = filtered_nodes
        .iter()
        .copied()
        .map(|node| (node.id.as_str(), node))
        .collect();
    Ok(flow_diagram_view(
        &artifact,
        mode,
        &by_id,
        &filtered_edges,
        std::slice::from_ref(selected_flow),
    ))
}

/// Compile both the official [`SpecBundle`] and its [`SpecViewBundle`] from a
/// single R-INT-5 and curation projection and a single production of each
/// artifact, binding view `content_id`s to the bundle artifacts by
/// construction (US-0036, AC-0246, AC-0247).
pub fn compile_spec_with_views(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> (SpecBundle, SpecViewBundle) {
    compile_spec_with_views_and_coverage(nodes, edges, flows, mode, rejected_hashes, &[])
}

/// [`compile_spec_with_views`] with the stored coverage inventories the
/// bundle's `coverage` and `coverage-index` artifacts project (AC-0592). The
/// views are unchanged by them.
///
/// # Panics
///
/// When the export's citations do not resolve (see
/// [`try_compile_spec_with_views_and_coverage`]).
pub fn compile_spec_with_views_and_coverage(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
    coverage: &[core_graph::coverage::StoredCoverage],
) -> (SpecBundle, SpecViewBundle) {
    crate::workbench::resolved(try_compile_spec_with_views_and_coverage(
        nodes,
        edges,
        flows,
        mode,
        rejected_hashes,
        coverage,
    ))
}

/// [`compile_spec_with_views_and_coverage`], failing closed instead of
/// panicking when the export's citations do not resolve (ADR-0058 decision
/// 6, AC-0595), exactly as [`crate::try_compile_spec_with_coverage`] does.
pub fn try_compile_spec_with_views_and_coverage(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
    coverage: &[core_graph::coverage::StoredCoverage],
) -> Result<(SpecBundle, SpecViewBundle), crate::FactIndexError> {
    try_compile_spec_with_views_supplied(
        nodes,
        edges,
        flows,
        mode,
        rejected_hashes,
        crate::Supplied {
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

/// [`try_compile_spec_with_views_and_coverage`] with everything the host
/// supplies beside the graph, exactly as [`crate::try_compile_spec_supplied`]
/// takes it (SPEC-12, AC-0653). The views are unchanged by it.
pub fn try_compile_spec_with_views_supplied(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
    supplied: crate::Supplied<'_>,
) -> Result<(SpecBundle, SpecViewBundle), crate::FactIndexError> {
    try_compile_spec_with_views_supplied_kept(
        nodes,
        edges,
        flows,
        mode,
        rejected_hashes,
        supplied,
        |artifact| artifact,
    )
}

/// [`try_compile_spec_with_views_supplied`] keeping `keep(artifact)` of each
/// artifact as it is compiled, as [`crate::try_compile_spec_supplied_kept`]
/// does (#2215). The views bind to each artifact's id, schema and content
/// id, which are read before `keep` takes it, so they are the same.
pub fn try_compile_spec_with_views_supplied_kept<A>(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
    supplied: crate::Supplied<'_>,
    mut keep: impl FnMut(SpecArtifact) -> A,
) -> Result<(SpecBundle<A>, SpecViewBundle), crate::FactIndexError> {
    // As every compile under test does, read a compacted base (#2215).
    #[cfg(test)]
    let compacted = crate::workbench::compacted_for_tests(nodes, edges);
    #[cfg(test)]
    let (nodes, edges) = (compacted.0.as_slice(), compacted.1.as_slice());
    let projection = selected::project(nodes, edges, flows, mode, rejected_hashes, None)
        .expect("unbounded ADR derivation");
    let beside = selected::Beside::new(nodes, edges, mode, rejected_hashes, supplied);
    let filtered_nodes = filter_nodes(&projection.nodes, mode, rejected_hashes);
    let filtered_edges = filter_edges(&projection.edges, mode, rejected_hashes);
    let architecture_model = crate::workbench::architecture::Model::new(
        &filtered_nodes,
        &filtered_edges,
        &projection.flows,
        &crate::workbench::architecture::Supplied {
            knowledge: beside.knowledge,
            repo_names: beside.repo_names,
            mode,
        },
    );
    // What the views bind to of each artifact: its identity, without its
    // content or assertions.
    let mut headers = Vec::new();
    let bundle = selected::try_bundle_from_filtered_kept(
        &filtered_nodes,
        &filtered_edges,
        &projection.flows,
        mode,
        rejected_hashes,
        beside,
        |artifact| {
            headers.push(SpecArtifact {
                id: artifact.id.clone(),
                file_name: String::new(),
                title: String::new(),
                format: String::new(),
                schema: artifact.schema.clone(),
                content_id: artifact.content_id.clone(),
                content: String::new(),
                assertions: Vec::new(),
            });
            keep(artifact)
        },
    )?;

    let artifact_for = |id: &str| -> &SpecArtifact {
        headers
            .iter()
            .find(|artifact| artifact.id == id)
            .expect("known official artifact id")
    };
    let topology_artifact = artifact_for("topology");
    let flow_artifact = artifact_for("flow-dossiers");
    let data_artifact = artifact_for("data-model");
    let architecture = crate::workbench::architecture::view(
        &architecture_model,
        artifact_for("architecture"),
        mode,
    );

    let by_id: BTreeMap<&str, &Node> = filtered_nodes
        .iter()
        .copied()
        .map(|node| (node.id.as_str(), node))
        .collect();

    // 1. Topology + deployment view: all projected Resource nodes (including
    // unresolved placeholder/Gap resources), backed Channel nodes, and any
    // explicit Gap endpoints touched by topology edges.
    let mut topology_edges: Vec<Edge> = filtered_edges
        .iter()
        .copied()
        .filter(|edge| TOPOLOGY_EDGE_LABELS.contains(&edge.label.as_str()))
        .cloned()
        .collect();
    let backed_channels: BTreeSet<&str> = topology_edges
        .iter()
        .filter(|edge| edge.label == "BACKS")
        .map(|edge| edge.dst.as_str())
        .collect();
    let configured_files: BTreeSet<&str> = topology_edges
        .iter()
        .filter(|edge| edge.label == "CONFIGURED_IN")
        .map(|edge| edge.dst.as_str())
        .collect();
    let mut topology_node_ids: BTreeSet<String> = filtered_nodes
        .iter()
        .copied()
        .filter(|node| {
            node.label == "Resource"
                || (node.label == "Channel" && backed_channels.contains(node.id.as_str()))
                || (node.label == "File" && configured_files.contains(node.id.as_str()))
        })
        .map(|node| node.id.clone())
        .collect();
    for edge in &topology_edges {
        if topology_node_ids.contains(edge.src.as_str())
            && by_id
                .get(edge.dst.as_str())
                .is_some_and(|dst| dst.label == "Gap")
        {
            topology_node_ids.insert(edge.dst.clone());
        }
    }
    topology_edges.retain(|edge| {
        topology_node_ids.contains(edge.src.as_str())
            && topology_node_ids.contains(edge.dst.as_str())
    });
    let topology_nodes: Vec<Node> = topology_node_ids
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).copied().cloned())
        .collect();

    // 3. Data-model view: DataEntity nodes and READS/WRITES/MAPS_TO relations.
    let entity_ids: BTreeSet<&str> = filtered_nodes
        .iter()
        .copied()
        .filter(|node| node.label == "DataEntity")
        .map(|node| node.id.as_str())
        .collect();
    let data_edges: Vec<Edge> = filtered_edges
        .iter()
        .copied()
        .filter(|edge| DATA_EDGE_LABELS.contains(&edge.label.as_str()))
        .filter(|edge| {
            entity_ids.contains(edge.src.as_str()) || entity_ids.contains(edge.dst.as_str())
        })
        .filter(|edge| {
            by_id.contains_key(edge.src.as_str()) && by_id.contains_key(edge.dst.as_str())
        })
        .cloned()
        .collect();
    let data_node_ids: BTreeSet<&str> = entity_ids
        .iter()
        .copied()
        .chain(
            data_edges
                .iter()
                .flat_map(|edge| [edge.src.as_str(), edge.dst.as_str()]),
        )
        .collect();
    let data_nodes: Vec<Node> = data_node_ids
        .iter()
        .filter_map(|id| by_id.get(id).copied().cloned())
        .collect();

    let views = SpecViewBundle {
        mode,
        topology: bounded_diagram_view(
            topology_artifact,
            mode,
            topology_nodes,
            topology_edges,
            Vec::new(),
            true,
        ),
        flows: flow_diagram_view(
            flow_artifact,
            mode,
            &by_id,
            &filtered_edges,
            &projection.flows,
        ),
        data_model: bounded_diagram_view(
            data_artifact,
            mode,
            data_nodes,
            data_edges,
            Vec::new(),
            false,
        ),
        architecture,
    };
    Ok((bundle, views))
}

/// Compile structured diagram views for `topology`, `flow-dossiers`, and
/// `data-model` from the exact same R-INT-5 and curation projection as
/// [`crate::compile_spec`] (US-0036, AC-0246, AC-0247).
pub fn compile_spec_views(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> SpecViewBundle {
    compile_spec_with_views(nodes, edges, flows, mode, rejected_hashes).1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compile_spec, provenance};
    use core_prov::{EvidenceRef, Tier};
    use flowtracer::FlowStatus;

    fn prov(tier: Tier, confidence: ConfidenceTier, extractor: &str, hash: &str) -> Provenance {
        Provenance::new(
            tier,
            confidence,
            vec![EvidenceRef {
                repo: "local/shop".into(),
                path: "src/orders.ts".into(),
                byte_start: 12,
                byte_end: 48,
                commit_sha: "abc123".into(),
            }],
            extractor,
            hash.as_bytes(),
        )
        .unwrap()
    }

    fn fixture() -> (Vec<Node>, Vec<Edge>, Vec<Flow>) {
        let observed_prov = prov(
            Tier::Dynamic,
            ConfidenceTier::Confirmed,
            "t1.terraform-state",
            "obs:queue",
        );
        let nodes = vec![
            Node {
                id: "res:aws_sqs_queue.orders".into(),
                label: "Resource".into(),
                props: serde_json::json!({
                    "logical_id": "aws_sqs_queue.orders",
                    "type": "aws_sqs_queue",
                    "observed": { "url": "https://sqs.us-east-1.amazonaws.com/1/orders" },
                    "observed_prov": observed_prov,
                    "resolved_by": "state.json",
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.iac", "res:queue"),
                }),
            },
            Node {
                id: "res:aws_lambda_function.fulfill".into(),
                label: "Resource".into(),
                props: serde_json::json!({
                    "logical_id": "aws_lambda_function.fulfill",
                    "type": "aws_lambda_function",
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.iac", "res:fulfill"),
                }),
            },
            Node {
                id: "res:module.unlinked_dlq".into(),
                label: "Resource".into(),
                props: serde_json::json!({
                    "logical_id": "module.unlinked_dlq",
                    "placeholder": true,
                    "reason": "unresolved module output",
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Gap, "t0.iac", "res:dlq-gap"),
                }),
            },
            Node {
                id: "chan:sqs:orders".into(),
                label: "Channel".into(),
                props: serde_json::json!({
                    "identity": "sqs:orders",
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.events", "chan:orders"),
                }),
            },
            Node {
                id: "sym:orders#create".into(),
                label: "Symbol".into(),
                props: serde_json::json!({
                    "name": "createOrder",
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.ts", "sym:create"),
                }),
            },
            Node {
                id: "data:orders".into(),
                label: "DataEntity".into(),
                props: serde_json::json!({
                    "name": "orders_table",
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.sql", "data:orders"),
                }),
            },
        ];
        let edges = vec![
            Edge {
                src: "res:aws_sqs_queue.orders".into(),
                dst: "chan:sqs:orders".into(),
                label: "BACKS".into(),
                props: serde_json::json!({
                    "prov": prov(Tier::Dynamic, ConfidenceTier::Confirmed, "t1.terraform-state", "edge:backs"),
                }),
            },
            Edge {
                src: "res:aws_sqs_queue.orders".into(),
                dst: "res:aws_lambda_function.fulfill".into(),
                label: "TRIGGERS".into(),
                props: serde_json::json!({
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.iac", "edge:triggers"),
                }),
            },
            Edge {
                src: "res:aws_lambda_function.fulfill".into(),
                dst: "res:module.unlinked_dlq".into(),
                label: "REFERENCES".into(),
                props: serde_json::json!({
                    "reason": "unresolved module output",
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Gap, "t0.iac", "edge:ref-gap"),
                }),
            },
            Edge {
                src: "sym:orders#create".into(),
                dst: "data:orders".into(),
                label: "WRITES".into(),
                props: serde_json::json!({
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.ts", "edge:writes"),
                }),
            },
        ];
        let flows = vec![Flow {
            trigger: "ep:POST:/orders".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "POST /orders".into(),
            hops: vec![
                Hop {
                    label: "HANDLES".into(),
                    src: "ep:POST:/orders".into(),
                    dst: "sym:orders#create".into(),
                    src_name: "POST /orders".into(),
                    dst_name: "createOrder".into(),
                    tier: "Deterministic".into(),
                    confidence: "Confirmed".into(),
                    evidence: Some("src/orders.ts bytes 12..48".into()),
                    provenance: prov(
                        Tier::Deterministic,
                        ConfidenceTier::Confirmed,
                        "t0.ts",
                        "hop:handles",
                    ),
                    gap_reason: None,
                    attempted_tiers: vec![],
                },
                Hop {
                    label: "PUBLISHES".into(),
                    src: "sym:orders#create".into(),
                    dst: "chan:sqs:orders".into(),
                    src_name: "createOrder".into(),
                    dst_name: "sqs:orders".into(),
                    tier: "Agentic".into(),
                    confidence: "InferredWeak".into(),
                    evidence: Some("src/orders.ts bytes 12..48".into()),
                    provenance: prov(
                        Tier::Agentic,
                        ConfidenceTier::InferredWeak,
                        "t3.agent",
                        "hop:publishes-weak",
                    ),
                    gap_reason: None,
                    attempted_tiers: vec!["T0".into(), "T1".into(), "T2".into()],
                },
            ],
            status: FlowStatus::Inferred,
            score: 0.65,
            depth_limited: false,
        }];
        (nodes, edges, flows)
    }

    // AC-0246 / #1669: selection precedes limits without changing dossier identity.
    #[test]
    fn selected_late_flow_preserves_identity_mode_and_infrastructure() {
        let (nodes, edges, original) = fixture();
        let mut flows: Vec<_> = (0..SPEC_VIEW_MAX_FLOWS)
            .map(|index| {
                let mut flow = original[0].clone();
                flow.trigger = format!("ep:AAA-padding:{index:04}");
                flow.trigger_name = format!("Padding {index}");
                flow.hops.clear();
                flow
            })
            .collect();
        flows.extend(original.clone());
        for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
            let rejected = BTreeSet::new();
            let (bundle, overview) =
                compile_spec_with_views(&nodes, &edges, &flows, mode, &rejected);
            let artifact = bundle
                .artifacts
                .iter()
                .find(|a| a.id == "flow-dossiers")
                .unwrap();
            assert!(
                !overview
                    .flows
                    .flows
                    .iter()
                    .any(|f| f.trigger == original[0].trigger)
            );
            let selected = compile_selected_flow_view(
                &nodes,
                &edges,
                &flows,
                mode,
                &rejected,
                &original[0].trigger,
                &artifact.content_id,
            )
            .unwrap();
            assert!(selected.matches_artifact(artifact));
            assert_eq!(selected.flows_total, 1);
            assert_eq!(selected.flows.len(), 1);
            let baseline = compile_spec_views(&nodes, &edges, &original, mode, &rejected).flows;
            assert_eq!(selected.nodes, baseline.nodes);
            assert_eq!(selected.edges, baseline.edges);
            assert_eq!(
                serde_json::to_value(&selected.flows).unwrap(),
                serde_json::to_value(&baseline.flows).unwrap()
            );
            assert_eq!(selected.deployments, baseline.deployments);
            assert!(
                compile_selected_flow_view(
                    &nodes,
                    &edges,
                    &flows,
                    mode,
                    &rejected,
                    &original[0].trigger,
                    "stale"
                )
                .is_err()
            );
            assert!(
                compile_selected_flow_view(
                    &nodes,
                    &edges,
                    &flows,
                    mode,
                    &rejected,
                    "missing-trigger",
                    &artifact.content_id
                )
                .is_err()
            );
        }
    }

    // AC-0246 (T-0246): compiled topology, flow, deployment, and data-model
    // views carry structured graph facts and match the exported projection's
    // schema and content identity.
    #[test]
    fn compiled_views_match_exported_artifact_identities_and_include_deployment_roles() {
        let (nodes, edges, flows) = fixture();
        for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
            let bundle = compile_spec(&nodes, &edges, &flows, mode, &BTreeSet::new());
            let views = compile_spec_views(&nodes, &edges, &flows, mode, &BTreeSet::new());

            let topology_art = bundle
                .artifacts
                .iter()
                .find(|a| a.id == "topology")
                .unwrap();
            let flow_art = bundle
                .artifacts
                .iter()
                .find(|a| a.id == "flow-dossiers")
                .unwrap();
            let data_art = bundle
                .artifacts
                .iter()
                .find(|a| a.id == "data-model")
                .unwrap();

            assert!(views.topology.matches_artifact(topology_art));
            assert!(views.flows.matches_artifact(flow_art));
            assert!(views.data_model.matches_artifact(data_art));

            // Deployment role from T1 Terraform state is attached to the SQS resource
            // and links it to the backed Channel in both topology and flow views.
            assert_eq!(views.topology.deployments.len(), 1);
            let deploy = &views.topology.deployments[0];
            assert_eq!(deploy.resource_id, "res:aws_sqs_queue.orders");
            assert_eq!(deploy.source, "t1.terraform-state");
            assert_eq!(deploy.backed_channels, vec!["chan:sqs:orders"]);
            assert_eq!(
                deploy.provenance.as_ref().map(|p| p.tier),
                Some(Tier::Dynamic)
            );

            // Data model view carries DataEntity + writing Symbol and WRITES edge.
            assert_eq!(views.data_model.nodes.len(), 2);
            assert_eq!(views.data_model.edges.len(), 1);
            assert_eq!(views.data_model.edges[0].label, "WRITES");

            // Under the per-view caps, views report truncated = false and totals equal lengths.
            for view in [&views.topology, &views.flows, &views.data_model] {
                assert!(!view.truncated);
                assert_eq!(view.nodes_total, view.nodes.len());
                assert_eq!(view.edges_total, view.edges.len());
                assert_eq!(view.flows_total, view.flows.len());
            }
        }

        // In BestEffort mode, the flow touches chan:sqs:orders, so the backing
        // deployed Resource (`res:aws_sqs_queue.orders`) joins the flow view's
        // infrastructure context.
        let best = compile_spec_views(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        assert!(
            best.flows
                .nodes
                .iter()
                .any(|node| node.id == "res:aws_sqs_queue.orders")
        );
        assert_eq!(best.flows.deployments.len(), 1);
    }

    // AC-0247 (T-0247): every node and edge in a compiled view preserves its
    // tier, confidence, and evidence, and Gap/partial hops remain visibly distinct.
    #[test]
    fn compiled_views_keep_gaps_and_partial_hops_distinct_with_provenance() {
        let (nodes, edges, flows) = fixture();
        let verified = compile_spec_views(
            &nodes,
            &edges,
            &flows,
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );

        // Unresolved placeholder resource and Gap REFERENCES edge remain in the
        // topology view with ConfidenceTier::Gap so the diagram renders them distinctly.
        let dlq = verified
            .topology
            .nodes
            .iter()
            .find(|node| node.id == "res:module.unlinked_dlq")
            .expect("unresolved resource retained");
        assert_eq!(
            provenance(&dlq.props, &dlq.id).confidence_tier,
            ConfidenceTier::Gap
        );
        let gap_edge = verified
            .topology
            .edges
            .iter()
            .find(|edge| edge.label == "REFERENCES")
            .expect("gap topology edge retained");
        assert_eq!(
            provenance(&gap_edge.props, "ref").confidence_tier,
            ConfidenceTier::Gap
        );

        // In VerifiedOnly mode, the InferredWeak PUBLISHES hop becomes an explicit
        // UNRESOLVED Gap hop and downgrades the flow status to Partial (R-INT-4, R-INT-5).
        assert_eq!(verified.flows.flows.len(), 1);
        let projected_flow = &verified.flows.flows[0];
        assert_eq!(projected_flow.status, FlowStatus::Partial);
        assert_eq!(projected_flow.hops[0].confidence, "Confirmed");
        assert_eq!(projected_flow.hops[1].confidence, "Gap");
        assert_eq!(projected_flow.hops[1].label, "UNRESOLVED");
        assert_eq!(
            projected_flow.hops[1].gap_reason.as_deref(),
            Some("weak inference excluded by verified-only export")
        );
        let gap_target = verified
            .flows
            .nodes
            .iter()
            .find(|node| node.id == projected_flow.hops[1].dst)
            .expect("synthetic projection gap node");
        assert_eq!(gap_target.label, "Gap");
        assert_eq!(
            provenance(&gap_target.props, &gap_target.id).confidence_tier,
            ConfidenceTier::Gap
        );
    }

    // AC-0246 (T-0246): graphs above SPEC_VIEW_MAX_NODES, SPEC_VIEW_MAX_EDGES,
    // or SPEC_VIEW_MAX_FLOWS are deterministically truncated to the exact caps,
    // report true pre-truncation totals with truncated = true, retain only edges
    // whose both endpoints survive the node cap, and keep artifact identity intact.
    #[test]
    fn views_above_node_edge_and_flow_caps_truncate_deterministically_and_keep_identity() {
        let node_total = SPEC_VIEW_MAX_NODES + 12;
        let mut nodes: Vec<Node> = (0..node_total)
            .map(|idx| {
                let id = format!("res:svc_{idx:04}");
                Node {
                    id: id.clone(),
                    label: "Resource".into(),
                    props: serde_json::json!({
                        "logical_id": format!("svc_{idx:04}"),
                        "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.iac", &id),
                    }),
                }
            })
            .collect();
        nodes.push(Node {
            id: "data:orders".into(),
            label: "DataEntity".into(),
            props: serde_json::json!({
                "name": "orders",
                "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.sql", "data:orders"),
            }),
        });
        let data_writers = 65usize;
        for idx in 0..data_writers {
            let id = format!("sym:writer_{idx:04}");
            nodes.push(Node {
                id: id.clone(),
                label: "Symbol".into(),
                props: serde_json::json!({
                    "name": format!("writer_{idx:04}"),
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.ts", &id),
                }),
            });
        }

        // Topology edges: 200 edges between surviving nodes (0..200 -> 1..201)
        // plus 12 edges whose destination is beyond SPEC_VIEW_MAX_NODES.
        let mut edges: Vec<Edge> = (0..200)
            .map(|idx| {
                let src = format!("res:svc_{idx:04}");
                let dst = format!("res:svc_{:04}", idx + 1);
                let hash = format!("topo:{src}->{dst}");
                Edge {
                    src,
                    dst,
                    label: "TRIGGERS".into(),
                    props: serde_json::json!({
                        "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.iac", &hash),
                    }),
                }
            })
            .collect();
        for idx in SPEC_VIEW_MAX_NODES..node_total {
            let src = "res:svc_0000".to_string();
            let dst = format!("res:svc_{idx:04}");
            let hash = format!("topo-overflow:{src}->{dst}");
            edges.push(Edge {
                src,
                dst,
                label: "REFERENCES".into(),
                props: serde_json::json!({
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.iac", &hash),
                }),
            });
        }

        // Data-model edges: exceed SPEC_VIEW_MAX_EDGES while keeping nodes well under SPEC_VIEW_MAX_NODES.
        let data_edge_total = SPEC_VIEW_MAX_EDGES + 25;
        for idx in 0..data_edge_total {
            let writer = format!("sym:writer_{:04}", idx % data_writers);
            let label = match idx % 3 {
                0 => "READS",
                1 => "WRITES",
                _ => "MAPS_TO",
            };
            // Make each (src, dst, label) edge distinct by adding DataEntity targets.
            let entity_id = format!("data:entity_{:04}", idx / 3);
            if idx % 3 == 0 {
                nodes.push(Node {
                    id: entity_id.clone(),
                    label: "DataEntity".into(),
                    props: serde_json::json!({
                        "name": entity_id,
                        "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.sql", &entity_id),
                    }),
                });
            }
            let hash = format!("data:{writer}:{label}:{entity_id}");
            edges.push(Edge {
                src: writer,
                dst: entity_id,
                label: label.into(),
                props: serde_json::json!({
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "t0.ts", &hash),
                }),
            });
        }

        // Flows: exceed SPEC_VIEW_MAX_FLOWS.
        let flow_total = SPEC_VIEW_MAX_FLOWS + 9;
        let flows: Vec<Flow> = (0..flow_total)
            .map(|idx| {
                let trigger = format!("ep:GET:/items/{idx:04}");
                let dst = format!("sym:writer_{:04}", idx % data_writers);
                let hash = format!("flow:{trigger}->{dst}");
                Flow {
                    trigger: trigger.clone(),
                    trigger_kind: "Endpoint".into(),
                    trigger_name: format!("GET /items/{idx:04}"),
                    hops: vec![Hop {
                        label: "HANDLES".into(),
                        src: trigger,
                        dst,
                        src_name: format!("GET /items/{idx:04}"),
                        dst_name: format!("writer_{:04}", idx % data_writers),
                        tier: "Deterministic".into(),
                        confidence: "Confirmed".into(),
                        evidence: None,
                        provenance: prov(
                            Tier::Deterministic,
                            ConfidenceTier::Confirmed,
                            "t0.ts",
                            &hash,
                        ),
                        gap_reason: None,
                        attempted_tiers: Vec::new(),
                    }],
                    status: FlowStatus::Verified,
                    score: 1.0,
                    depth_limited: false,
                }
            })
            .collect();

        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let views = compile_spec_views(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );

        // 1. Topology view: nodes capped to SPEC_VIEW_MAX_NODES, edges to dropped nodes removed.
        assert_eq!(views.topology.nodes.len(), SPEC_VIEW_MAX_NODES);
        assert_eq!(views.topology.nodes_total, node_total);
        assert_eq!(views.topology.edges_total, 212);
        assert_eq!(views.topology.edges.len(), 200);
        assert!(views.topology.truncated);
        let retained_topo_ids: BTreeSet<&str> = views
            .topology
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect();
        assert!(views.topology.edges.iter().all(|edge| {
            retained_topo_ids.contains(edge.src.as_str())
                && retained_topo_ids.contains(edge.dst.as_str())
        }));

        // 2. Data-model view: edges capped to SPEC_VIEW_MAX_EDGES, every retained edge has both endpoints.
        assert_eq!(views.data_model.edges.len(), SPEC_VIEW_MAX_EDGES);
        assert_eq!(views.data_model.edges_total, data_edge_total);
        assert!(views.data_model.truncated);
        let retained_data_ids: BTreeSet<&str> = views
            .data_model
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect();
        assert!(views.data_model.edges.iter().all(|edge| {
            retained_data_ids.contains(edge.src.as_str())
                && retained_data_ids.contains(edge.dst.as_str())
        }));

        // 3. Flow view: flows capped to SPEC_VIEW_MAX_FLOWS.
        assert_eq!(views.flows.flows.len(), SPEC_VIEW_MAX_FLOWS);
        assert_eq!(views.flows.flows_total, flow_total);
        assert!(views.flows.truncated);
        let retained_flow_ids: BTreeSet<&str> = views
            .flows
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect();
        assert!(views.flows.edges.iter().all(|edge| {
            retained_flow_ids.contains(edge.src.as_str())
                && retained_flow_ids.contains(edge.dst.as_str())
        }));
        // Participants come only from retained flows: every retained hop keeps
        // both endpoints, and a dropped flow's trigger is absent (#591).
        assert!(
            views
                .flows
                .flows
                .iter()
                .flat_map(|flow| &flow.hops)
                .all(|hop| {
                    retained_flow_ids.contains(hop.src.as_str())
                        && retained_flow_ids.contains(hop.dst.as_str())
                })
        );
        for idx in SPEC_VIEW_MAX_FLOWS..flow_total {
            let dropped = format!("ep:GET:/items/{idx:04}");
            assert!(!retained_flow_ids.contains(dropped.as_str()));
        }

        // Content identities and schemas remain bound to the full artifacts.
        let topology_art = bundle
            .artifacts
            .iter()
            .find(|a| a.id == "topology")
            .unwrap();
        let flow_art = bundle
            .artifacts
            .iter()
            .find(|a| a.id == "flow-dossiers")
            .unwrap();
        let data_art = bundle
            .artifacts
            .iter()
            .find(|a| a.id == "data-model")
            .unwrap();
        assert!(views.topology.matches_artifact(topology_art));
        assert!(views.flows.matches_artifact(flow_art));
        assert!(views.data_model.matches_artifact(data_art));
    }

    // AC-0247 (T-0247): unrecognized participant id prefixes map to "Unknown"
    // rather than mislabeling unknown participants as "Symbol".
    #[test]
    fn unknown_participant_prefix_maps_to_unknown_not_symbol() {
        assert_eq!(participant_label("sym:orders#create", None), "Symbol");
        assert_eq!(participant_label("ep:POST:/orders", None), "Endpoint");
        assert_eq!(participant_label("gap:missing", None), "Gap");
        assert_eq!(participant_label("custom:worker", Some("Gap")), "Gap");
        assert_eq!(participant_label("custom:worker", None), "Unknown");
        assert_eq!(participant_label("unprefixed_participant", None), "Unknown");

        let flows = vec![Flow {
            trigger: "custom:trigger".into(),
            trigger_kind: "Custom".into(),
            trigger_name: "Custom trigger".into(),
            hops: vec![Hop {
                label: "DISPATCHES".into(),
                src: "custom:trigger".into(),
                dst: "mystery:worker".into(),
                src_name: "Custom trigger".into(),
                dst_name: "Mystery worker".into(),
                tier: "Deterministic".into(),
                confidence: "Confirmed".into(),
                evidence: None,
                provenance: prov(
                    Tier::Deterministic,
                    ConfidenceTier::Confirmed,
                    "t0.custom",
                    "hop:custom",
                ),
                gap_reason: None,
                attempted_tiers: Vec::new(),
            }],
            status: FlowStatus::Verified,
            score: 1.0,
            depth_limited: false,
        }];
        let views = compile_spec_views(&[], &[], &flows, ExportMode::BestEffort, &BTreeSet::new());
        for id in ["custom:trigger", "mystery:worker"] {
            let node = views
                .flows
                .nodes
                .iter()
                .find(|node| node.id == id)
                .expect("synthetic participant node");
            assert_eq!(node.label, "Unknown");
        }
    }

    // AC-0246 (T-0246): compile_spec_with_views produces both the SpecBundle and
    // SpecViewBundle from one projection, with view content_ids matching the
    // bundle artifacts' content_ids and output matching compile_spec_views.
    #[test]
    fn compile_spec_with_views_reuses_bundle_content_ids_and_matches_compile_spec_views() {
        let (nodes, edges, flows) = fixture();
        let rejected = BTreeSet::from([prov(
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
            "t3.agent",
            "edge:weak-reads",
        )
        .content_hash]);

        for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
            for rejected_set in [BTreeSet::new(), rejected.clone()] {
                let (bundle, views) =
                    compile_spec_with_views(&nodes, &edges, &flows, mode, &rejected_set);
                let expected_bundle = compile_spec(&nodes, &edges, &flows, mode, &rejected_set);
                let expected_views =
                    compile_spec_views(&nodes, &edges, &flows, mode, &rejected_set);

                assert_eq!(
                    serde_json::to_value(&bundle).unwrap(),
                    serde_json::to_value(&expected_bundle).unwrap(),
                );
                assert_eq!(
                    serde_json::to_value(&views).unwrap(),
                    serde_json::to_value(&expected_views).unwrap(),
                );

                for (artifact_id, view) in [
                    ("topology", &views.topology),
                    ("flow-dossiers", &views.flows),
                    ("data-model", &views.data_model),
                ] {
                    let artifact = bundle
                        .artifacts
                        .iter()
                        .find(|item| item.id == artifact_id)
                        .expect("official artifact in bundle");
                    assert_eq!(view.content_id, artifact.content_id);
                    assert_eq!(view.schema, artifact.schema);
                    assert!(view.matches_artifact(artifact));
                }
            }
        }
    }
}
