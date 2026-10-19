//! Deterministic full-spec bundle for the M9 Spec Workbench (US-0012).

use crate::{TOPOLOGY_EDGE_LABELS, flow_dossier, topology_mermaid};
use core_graph::omission::{FillSeam, Omission, OmissionClass};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Provenance, Tier};
use flowtracer::{Flow, FlowStatus, Hop};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

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
        let bytes = serde_json::to_vec(&(
            &self.schema,
            &self.id,
            &self.file_name,
            &self.title,
            &self.format,
            &self.content,
            &self.assertions,
        ))
        .expect("spec artifact serializes");
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"cartograph:spec-artifact:v1\0");
        hasher.update(&bytes);
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

/// Full official spec export returned to the Workbench.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecBundle {
    /// R-INT-5 projection applied consistently to all artifacts.
    pub mode: ExportMode,
    /// The complete official artifact set in stable order.
    pub artifacts: Vec<SpecArtifact>,
    /// Number of visible graph-backed assertions across artifacts.
    pub assertion_count: usize,
    /// Explicit unresolved assertions in the Gap register.
    pub gap_count: usize,
    /// Explicit ADR/code conflicts in the Drift register.
    pub drift_count: usize,
    /// Explicit auth/IAM findings in the security view.
    pub security_count: usize,
}

fn fallback_provenance(identity: &str) -> Provenance {
    Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Gap,
        vec![],
        "spec.invalid-provenance",
        identity.as_bytes(),
    )
    .expect("Gap is within the deterministic confidence ceiling")
}

/// The validated provenance of a fact, or the explicit-Gap fallback when a
/// fact carries none. Public for the same reason as the register predicates
/// (#116): tier tallies must count with one definition on every surface.
pub fn provenance(props: &serde_json::Value, identity: &str) -> Provenance {
    serde_json::from_value::<Provenance>(props["prov"].clone())
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

fn included(provenance: &Provenance, mode: ExportMode, rejected_hashes: &BTreeSet<String>) -> bool {
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

fn node_name(node: &Node) -> String {
    if node.label == "Endpoint"
        && let (Some(method), Some(path)) =
            (node.props["method"].as_str(), node.props["path"].as_str())
    {
        return format!("{method} {path}");
    }
    text_prop(
        node,
        &[
            "title",
            "name",
            "identity",
            "logical_id",
            "route",
            "path",
            "reason",
        ],
    )
    .map(String::from)
    .unwrap_or_else(|| node.id.clone())
}

/// The relation a Gap node's escalation would need the bounded T3 broker to
/// propose — mirrors `task_evidence::planning::plan`'s adjacent-slot lookup
/// exactly (dst-first, then src, then the node's own stored `edge_label`
/// prop) so the register's offer and the broker's actual attempt never
/// disagree about which relation is in play (#238). Not a proof of
/// direction/cardinality, same as that lookup. Leaves the final `"CALLS"`
/// default out (unlike planning): `None` here reads as "unknown" to the
/// register, and stays offered rather than being pinned to a guess.
fn gap_node_edge_label<'a>(node: &'a Node, edges: &[&'a Edge]) -> Option<&'a str> {
    edges
        .iter()
        .find(|edge| edge.dst == node.id)
        .or_else(|| edges.iter().find(|edge| edge.src == node.id))
        .map(|edge| edge.label.as_str())
        .or_else(|| node.props["edge_label"].as_str())
}

fn node_assertion(node: &Node, edge_label: Option<&str>) -> SpecAssertion {
    let provenance = provenance(&node.props, &format!("node:{}", node.id));
    let name = node_name(node);
    SpecAssertion {
        id: format!("node:{}", node.id),
        subject_id: node.id.clone(),
        subject_kind: node.label.clone(),
        summary: format!("{}: {name}", node.label),
        omission: gap_omission(&provenance, &node.props, &name),
        provenance,
        edge_label: edge_label.map(String::from),
    }
}

fn edge_identity(edge: &Edge) -> String {
    format!("{} {} {}", edge.src, edge.label, edge.dst)
}

fn edge_assertion(edge: &Edge) -> SpecAssertion {
    let identity = edge_identity(edge);
    let provenance = provenance(&edge.props, &format!("edge:{identity}"));
    // Gap edges carry the same `reason` as their gap node (#241) — surface it
    // so register rows never render a bare identity in the reason column.
    // Non-gap edges (topology, mappings, decisions) keep their identity.
    let summary = if provenance.confidence_tier == ConfidenceTier::Gap
        && let Some(reason) = edge.props["reason"].as_str()
    {
        format!("{}: {reason}", edge.label)
    } else {
        identity.clone()
    };
    SpecAssertion {
        id: format!("edge:{identity}"),
        omission: gap_omission(
            &provenance,
            &edge.props,
            &format!("unresolved {} edge", edge.label),
        ),
        subject_id: identity,
        subject_kind: edge.label.clone(),
        summary,
        provenance,
        edge_label: Some(edge.label.clone()),
    }
}

fn hop_assertion(flow: &Flow, hop: &Hop, index: usize) -> SpecAssertion {
    SpecAssertion {
        id: format!("flow:{}:{index}:{}:{}", flow.trigger, hop.label, hop.dst),
        subject_id: format!("{} {} {}", hop.src, hop.label, hop.dst),
        subject_kind: "FlowHop".into(),
        summary: format!("{}: {} → {}", hop.label, hop.src_name, hop.dst_name),
        omission: gap_omission(
            &hop.provenance,
            &serde_json::json!({"reason": hop.gap_reason}),
            &format!("unresolved {} hop", hop.label),
        ),
        provenance: hop.provenance.clone(),
        // A flow-hop row never opens the Resolution Strategy modal (App.tsx
        // only escalates `node:`-prefixed assertions), so it carries no
        // escalation-capability signal.
        edge_label: None,
    }
}

pub(crate) fn markdown_safe(value: &str) -> String {
    value.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn evidence_text(provenance: &Provenance) -> String {
    if provenance.evidence.is_empty() {
        return "—".into();
    }
    provenance
        .evidence
        .iter()
        .map(|evidence| {
            format!(
                "{}:{} bytes {}..{} @ {}",
                evidence.repo,
                evidence.path,
                evidence.byte_start,
                evidence.byte_end,
                evidence.commit_sha
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub(crate) fn append_assertions(content: &mut String, assertions: &[SpecAssertion]) {
    content.push_str("\n## Assertions and inline provenance\n\n");
    if assertions.is_empty() {
        content.push_str("No graph-backed assertions were recovered for this artifact.\n");
        return;
    }
    content.push_str(
        "| Assertion | Tier | Confidence | Method | Evidence | Extractor | Content hash |\n\
         |---|---|---|---|---|---|---|\n",
    );
    for assertion in assertions {
        let provenance = &assertion.provenance;
        writeln!(
            content,
            "| {} | {:?} | {:?} | {} | {} | `{}` | `{}` |",
            markdown_safe(&assertion.summary),
            provenance.tier,
            provenance.confidence_tier,
            markdown_safe(&provenance.method.label()),
            markdown_safe(&evidence_text(provenance)),
            markdown_safe(&provenance.extractor_id),
            markdown_safe(&provenance.content_hash),
        )
        .expect("write to string");
    }
}

fn artifact(
    id: &str,
    file_name: &str,
    title: &str,
    format: &str,
    mut content: String,
    assertions: Vec<SpecAssertion>,
) -> SpecArtifact {
    append_assertions(&mut content, &assertions);
    identified_artifact(id, file_name, title, format, content, assertions)
}

/// An artifact whose content is final, stamped with schema and identity.
fn identified_artifact(
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

fn filter_nodes<'a>(
    nodes: &'a [Node],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> Vec<&'a Node> {
    let mut selected: Vec<&Node> = nodes
        .iter()
        .filter(|node| included(&provenance(&node.props, &node.id), mode, rejected_hashes))
        .collect();
    selected.sort_by(|left, right| left.id.cmp(&right.id));
    selected
}

fn filter_edges<'a>(
    edges: &'a [Edge],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> Vec<&'a Edge> {
    let mut selected: Vec<&Edge> = edges
        .iter()
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

fn recovered_user_stories(nodes: &[&Node]) -> (String, Vec<SpecAssertion>) {
    let capabilities: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|node| node.label == "Capability")
        .filter(|node| provenance(&node.props, &node.id).confidence_tier != ConfidenceTier::Gap)
        .collect();
    let mut content = String::from("# Recovered user stories\n\n");
    if capabilities.is_empty() {
        content.push_str("No Capability facts have been recovered yet.\n");
    } else {
        for (index, capability) in capabilities.iter().enumerate() {
            writeln!(
                content,
                "## US-R-{:04} — {}\n\n- Recovered capability assertion: `{}`\n",
                index + 1,
                node_name(capability),
                capability.id,
            )
            .expect("write to string");
        }
    }
    let assertions = capabilities
        .into_iter()
        .map(|node| node_assertion(node, None))
        .collect();
    (content, assertions)
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

/// Structured index of every matrix link (`US-TM.json`).
#[derive(Serialize)]
struct MatrixIndex<'a> {
    schema: &'static str,
    mode: ExportMode,
    links: usize,
    classes: Vec<MatrixClassIndex<'a>>,
}

#[derive(Serialize)]
struct MatrixClassIndex<'a> {
    id: String,
    relation: &'a str,
    target: &'a str,
    links: usize,
    /// Assertion ids; each carries its full provenance in the bundle.
    members: Vec<&'a str>,
}

fn matrix_row(content: &mut String, edge: &Edge) {
    writeln!(
        content,
        "| `{}` | {} | `{}` |",
        markdown_safe(&edge.src),
        edge.label,
        markdown_safe(&edge.dst)
    )
    .expect("write to string");
}

/// The recovered traceability matrix plus its JSON index (#487). Past
/// [`US_TM_FLAT_LIMIT`] links the Markdown groups links by relation × target,
/// details at most [`US_TM_MAX_CLASSES`] classes with
/// [`US_TM_CLASS_REPRESENTATIVES`] links each, and counts every omission; the
/// returned assertions stay complete and the index lists every link by class.
fn traceability_matrix(edges: &[&Edge], mode: ExportMode) -> (String, String, Vec<SpecAssertion>) {
    let relevant: Vec<(&Edge, SpecAssertion)> = edges
        .iter()
        .copied()
        .filter(|edge| TRACE_LABELS.contains(&edge.label.as_str()))
        .filter(|edge| {
            provenance(&edge.props, &edge_identity(edge)).confidence_tier != ConfidenceTier::Gap
        })
        .map(|edge| (edge, edge_assertion(edge)))
        .collect();
    let classes = largest_groups(relevant.iter(), |(edge, _)| {
        (edge.label.as_str(), edge.dst.as_str())
    });
    let mut content = String::from("# Recovered US traceability matrix\n\n");
    if relevant.len() <= US_TM_FLAT_LIMIT {
        content.push_str("| Source | Relation | Target |\n|---|---|---|\n");
        for (edge, _) in &relevant {
            matrix_row(&mut content, edge);
        }
        if relevant.is_empty() {
            content.push_str("| — | No recovered mappings | — |\n");
        }
        let assertions: Vec<SpecAssertion> = relevant
            .iter()
            .map(|(_, assertion)| assertion.clone())
            .collect();
        append_assertions(&mut content, &assertions);
    } else {
        let shown = &classes[..classes.len().min(US_TM_MAX_CLASSES)];
        writeln!(
            content,
            "{} recovered links in {} relation × target classes, largest first. Each \
             class lists up to {US_TM_CLASS_REPRESENTATIVES} representative links; \
             `US-TM.json` lists every link by class, and each link keeps its inline \
             provenance in the bundle's assertions.\n",
            relevant.len(),
            classes.len(),
        )
        .expect("write to string");
        content.push_str("| Class | Relation | Target | Links |\n|---|---|---|---|\n");
        for (index, ((relation, target), members)) in shown.iter().enumerate() {
            writeln!(
                content,
                "| {} | {relation} | `{}` | {} |",
                matrix_class_id(index),
                markdown_safe(target),
                members.len(),
            )
            .expect("write to string");
        }
        let hidden = &classes[shown.len()..];
        if !hidden.is_empty() {
            writeln!(
                content,
                "| … | {} more classes, not detailed here — listed in `US-TM.json` | — | {} |",
                hidden.len(),
                hidden
                    .iter()
                    .map(|(_, members)| members.len())
                    .sum::<usize>(),
            )
            .expect("write to string");
        }
        let mut representatives: Vec<SpecAssertion> = Vec::new();
        for (index, ((relation, target), members)) in shown.iter().enumerate() {
            writeln!(
                content,
                "\n## {} — {relation} `{}` ({} links)\n\n| Source | Relation | Target |\n|---|---|---|",
                matrix_class_id(index),
                markdown_safe(target),
                members.len(),
            )
            .expect("write to string");
            for (edge, assertion) in members.iter().take(US_TM_CLASS_REPRESENTATIVES) {
                matrix_row(&mut content, edge);
                representatives.push(assertion.clone());
            }
            let more = members.len().saturating_sub(US_TM_CLASS_REPRESENTATIVES);
            if more > 0 {
                writeln!(
                    content,
                    "\n… {more} more links in this class — listed in `US-TM.json` under `{}`.",
                    matrix_class_id(index),
                )
                .expect("write to string");
            }
        }
        append_assertions(&mut content, &representatives);
        writeln!(
            content,
            "\nInline provenance is shown for the {} representative links above; \
             the other {} carry theirs in the bundle's structured assertions.",
            representatives.len(),
            relevant.len() - representatives.len(),
        )
        .expect("write to string");
    }
    let index = MatrixIndex {
        schema: "cartograph.us-tm-index/v1",
        mode,
        links: relevant.len(),
        classes: classes
            .iter()
            .enumerate()
            .map(|(index, ((relation, target), members))| MatrixClassIndex {
                id: matrix_class_id(index),
                relation,
                target,
                links: members.len(),
                members: members
                    .iter()
                    .map(|(_, assertion)| assertion.id.as_str())
                    .collect(),
            })
            .collect(),
    };
    let mut sidecar = serde_json::to_string(&index).expect("serialize matrix index");
    sidecar.push('\n');
    let assertions = relevant
        .into_iter()
        .map(|(_, assertion)| assertion)
        .collect();
    (content, sidecar, assertions)
}

fn matrix_class_id(index: usize) -> String {
    format!("M-{:02}", index + 1)
}

fn project_flows(
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
                            "inference rejected by Workbench curation"
                        } else {
                            "weak inference excluded by verified-only export"
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

fn flow_artifact(
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> (String, Vec<SpecAssertion>) {
    let projected = project_flows(flows, mode, rejected_hashes);
    let assertions = projected
        .iter()
        .flat_map(|flow| {
            flow.hops
                .iter()
                .enumerate()
                .map(|(index, hop)| hop_assertion(flow, hop, index))
        })
        .collect();
    (flow_dossier(&projected), assertions)
}

fn topology_artifact(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let mut topology_edges: Vec<Edge> = edges
        .iter()
        .filter(|edge| TOPOLOGY_EDGE_LABELS.contains(&edge.label.as_str()))
        .filter(|edge| {
            provenance(&edge.props, &edge_identity(edge)).confidence_tier != ConfidenceTier::Gap
        })
        .map(|edge| (*edge).clone())
        .collect();
    let backed_channels: BTreeSet<&str> = topology_edges
        .iter()
        .filter(|edge| edge.label == "BACKS")
        .map(|edge| edge.dst.as_str())
        .collect();
    let topology_nodes: Vec<Node> = nodes
        .iter()
        .filter(|node| {
            node.label == "Resource"
                || (node.label == "Channel" && backed_channels.contains(node.id.as_str()))
        })
        .filter(|node| provenance(&node.props, &node.id).confidence_tier != ConfidenceTier::Gap)
        .map(|node| (*node).clone())
        .collect();
    let topology_ids: BTreeSet<&str> = topology_nodes.iter().map(|node| node.id.as_str()).collect();
    topology_edges.retain(|edge| {
        topology_ids.contains(edge.src.as_str()) && topology_ids.contains(edge.dst.as_str())
    });
    let mut assertions: Vec<SpecAssertion> = topology_nodes
        .iter()
        .map(|node| node_assertion(node, None))
        .collect();
    assertions.extend(topology_edges.iter().map(edge_assertion));
    let diagram = topology_mermaid(&topology_nodes, &topology_edges);
    let content = format!("# Resource topology\n\n```mermaid\n{diagram}```\n");
    (content, assertions)
}

const DATA_EDGE_LABELS: &[&str] = &["READS", "WRITES", "MAPS_TO"];

fn data_model(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|node| (node.id.as_str(), *node)).collect();
    let entities: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|node| node.label == "DataEntity")
        .filter(|node| provenance(&node.props, &node.id).confidence_tier != ConfidenceTier::Gap)
        .collect();
    let entity_ids: BTreeSet<&str> = entities.iter().map(|node| node.id.as_str()).collect();
    let mappings: Vec<&Edge> = edges
        .iter()
        .copied()
        .filter(|edge| DATA_EDGE_LABELS.contains(&edge.label.as_str()))
        .filter(|edge| {
            provenance(&edge.props, &edge_identity(edge)).confidence_tier != ConfidenceTier::Gap
        })
        .filter(|edge| {
            entity_ids.contains(edge.src.as_str()) || entity_ids.contains(edge.dst.as_str())
        })
        .filter(|edge| {
            by_id.contains_key(edge.src.as_str()) && by_id.contains_key(edge.dst.as_str())
        })
        .collect();
    let model_ids: BTreeSet<&str> = entity_ids
        .iter()
        .copied()
        .chain(
            mappings
                .iter()
                .flat_map(|edge| [edge.src.as_str(), edge.dst.as_str()]),
        )
        .collect();
    let model_nodes: Vec<&Node> = model_ids
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .collect();
    let mut aliases = BTreeMap::new();
    for (index, node) in model_nodes.iter().enumerate() {
        aliases.insert(node.id.as_str(), format!("d{index}"));
    }
    let mut content = String::from("# Recovered data model\n\n```mermaid\nflowchart LR\n");
    for node in &model_nodes {
        let display = if node.label == "DataEntity" {
            node_name(node)
        } else {
            format!("{}: {}", node.label, node_name(node))
        };
        writeln!(
            content,
            "    {}[\"{}\"]",
            aliases[node.id.as_str()],
            display.replace(['\r', '\n'], " ").replace('"', "'")
        )
        .expect("write to string");
    }
    for edge in &mappings {
        if let (Some(source), Some(target)) = (
            aliases.get(edge.src.as_str()),
            aliases.get(edge.dst.as_str()),
        ) {
            writeln!(content, "    {source} -->|{}| {target}", edge.label)
                .expect("write to string");
        }
    }
    content.push_str("```\n");
    if entities.is_empty() {
        content.push_str("\nNo DataEntity facts have been recovered yet.\n");
    }
    if !mappings.is_empty() {
        content.push_str(
            "\n## Access and mapping relations\n\n| Source | Relation | Target |\n|---|---|---|\n",
        );
        for edge in &mappings {
            writeln!(
                content,
                "| `{}` | {} | `{}` |",
                markdown_safe(&edge.src),
                edge.label,
                markdown_safe(&edge.dst),
            )
            .expect("write to string");
        }
    }
    let mut assertions: Vec<SpecAssertion> = model_nodes
        .into_iter()
        .map(|node| node_assertion(node, None))
        .collect();
    assertions.extend(mappings.into_iter().map(edge_assertion));
    (content, assertions)
}

fn adr_set(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let adrs: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|node| node.label == "ADR")
        .filter(|node| provenance(&node.props, &node.id).confidence_tier != ConfidenceTier::Gap)
        .collect();
    let adr_ids: BTreeSet<&str> = adrs.iter().map(|node| node.id.as_str()).collect();
    let decisions: Vec<&Edge> = edges
        .iter()
        .copied()
        .filter(|edge| edge.label == "DECIDES")
        .filter(|edge| {
            provenance(&edge.props, &edge_identity(edge)).confidence_tier != ConfidenceTier::Gap
        })
        .filter(|edge| adr_ids.contains(edge.src.as_str()))
        .collect();
    let mut content = String::from("# Found and recovered ADRs\n\n");
    for adr in &adrs {
        writeln!(content, "## {}\n", node_name(adr)).expect("write to string");
        if let Some(origin) = adr.props["origin"].as_str() {
            writeln!(content, "**Origin:** {origin}\n").expect("write to string");
        }
        if let Some(status) = adr.props["status"].as_str() {
            writeln!(content, "**Status:** {status}\n").expect("write to string");
        }
        if let Some(body) = text_prop(adr, &["body", "content", "decision"]) {
            writeln!(content, "{body}\n").expect("write to string");
        }
    }
    if adrs.is_empty() {
        content.push_str("No found or recovered ADR facts have been recovered yet.\n");
    }
    if !decisions.is_empty() {
        content.push_str("\n## Decision links\n\n| ADR | Relation | Subject |\n|---|---|---|\n");
        for edge in &decisions {
            writeln!(
                content,
                "| `{}` | {} | `{}` |",
                markdown_safe(&edge.src),
                edge.label,
                markdown_safe(&edge.dst),
            )
            .expect("write to string");
        }
    }
    let mut assertions: Vec<SpecAssertion> = adrs
        .into_iter()
        .map(|node| node_assertion(node, None))
        .collect();
    assertions.extend(decisions.into_iter().map(edge_assertion));
    (content, assertions)
}

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

/// True when `edge` records ADR/code drift (see [`is_drift_node`]).
pub fn is_drift_edge(edge: &Edge) -> bool {
    matches!(edge.label.as_str(), "CONFLICTS" | "DRIFTS_FROM")
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

/// A gap's cause: a Gap node's stop reason, otherwise the unresolved
/// relation kind — the same key the Gaps lane groups by (AC-0082), so the
/// artifact's class counts reconcile with the UI's.
fn gap_cause(assertion: &SpecAssertion) -> String {
    if assertion.subject_kind == "Gap" {
        assertion
            .summary
            .strip_prefix("Gap: ")
            .unwrap_or(&assertion.summary)
            .to_string()
    } else {
        format!("unresolved {} edge", assertion.subject_kind)
    }
}

/// One cause class of the gap register.
struct GapClass<'a> {
    cause: String,
    extractor: &'a str,
    next_tier: &'static str,
    members: Vec<&'a SpecAssertion>,
}

/// Group gaps into cause classes (cause × extractor), largest first, then by
/// cause and extractor in byte order; members keep the register's id order.
fn gap_classes(assertions: &[SpecAssertion]) -> Vec<GapClass<'_>> {
    let mut classes: BTreeMap<(String, &str), GapClass<'_>> = BTreeMap::new();
    for assertion in assertions {
        let cause = gap_cause(assertion);
        let extractor = assertion.provenance.extractor_id.as_str();
        classes
            .entry((cause.clone(), extractor))
            .or_insert_with(|| GapClass {
                cause,
                extractor,
                next_tier: next_tier(&assertion.provenance),
                members: Vec::new(),
            })
            .members
            .push(assertion);
    }
    let mut classes: Vec<GapClass<'_>> = classes.into_values().collect();
    classes.sort_by(|left, right| {
        right
            .members
            .len()
            .cmp(&left.members.len())
            .then_with(|| left.cause.cmp(&right.cause))
            .then_with(|| left.extractor.cmp(right.extractor))
    });
    classes
}

fn class_id(index: usize) -> String {
    format!("C-{:02}", index + 1)
}

fn short_cause(cause: &str) -> String {
    if cause.chars().count() <= GAP_CAUSE_MAX_CHARS {
        return cause.to_string();
    }
    let mut short: String = cause.chars().take(GAP_CAUSE_MAX_CHARS).collect();
    short.push('…');
    short
}

/// Structured index of every register instance (`gap_register.json`).
#[derive(Serialize)]
struct GapRegisterIndex<'a> {
    schema: &'static str,
    mode: ExportMode,
    findings: usize,
    instances: usize,
    /// Register rows that restate a listed Gap node's finding — its
    /// supporting Gap edges and flow-hop restatements. They are not classes
    /// of their own, exactly as in the Gaps lane, but stay indexed here.
    supporting: Vec<&'a str>,
    /// Findings per omission class and filling seam (#585, AC-0254).
    by_omission: Vec<OmissionRollup>,
    classes: Vec<GapClassIndex<'a>>,
}

#[derive(Serialize)]
struct OmissionRollup {
    class: OmissionClass,
    seam: FillSeam,
    findings: usize,
}

/// Findings counted per (omission class, seam), in taxonomy order.
fn omission_rollup(findings: &[SpecAssertion]) -> Vec<OmissionRollup> {
    let mut counts: BTreeMap<(OmissionClass, FillSeam), usize> = BTreeMap::new();
    for finding in findings {
        if let Some(omission) = &finding.omission {
            *counts.entry((omission.class, omission.seam)).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .map(|((class, seam), findings)| OmissionRollup {
            class,
            seam,
            findings,
        })
        .collect()
}

fn render_omission_rollup(content: &mut String, rollup: &[OmissionRollup]) {
    if rollup.is_empty() {
        return;
    }
    content.push_str(
        "\n## By omission class\n\nWhy each finding is missing, and the seam that would fill it.\n\n\
         | Omission class | Filled by | Findings |\n|---|---|---|\n",
    );
    for row in rollup {
        writeln!(
            content,
            "| {} | {} | {} |",
            row.class.label(),
            row.seam.label(),
            row.findings
        )
        .expect("write to string");
    }
}

/// Reader labels for an assertion's omission cells.
fn omission_cells(omission: Option<&Omission>) -> (&'static str, &'static str) {
    omission.map_or(("—", "—"), |omission| {
        (omission.class.label(), omission.seam.label())
    })
}

/// A cause class's omission when every member shares it.
fn uniform_omission<'a>(members: &[&'a SpecAssertion]) -> Option<&'a Omission> {
    let first = members.first()?.omission.as_ref()?;
    members
        .iter()
        .all(|member| {
            member.omission.as_ref().is_some_and(|omission| {
                (omission.class, omission.seam) == (first.class, first.seam)
            })
        })
        .then_some(first)
}

#[derive(Serialize)]
struct GapClassIndex<'a> {
    id: String,
    cause: &'a str,
    extractor: &'a str,
    next_tier: &'static str,
    /// The class's omission when all members share it; members always carry
    /// their own in the bundle.
    #[serde(skip_serializing_if = "Option::is_none")]
    omission_class: Option<OmissionClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seam: Option<FillSeam>,
    instances: usize,
    /// Assertion ids; each carries its full provenance in the bundle.
    members: Vec<&'a str>,
}

fn register_row(content: &mut String, assertion: &SpecAssertion) {
    let (class, seam) = omission_cells(assertion.omission.as_ref());
    writeln!(
        content,
        "| `{}` | {} | {class} | {seam} |",
        markdown_safe(&assertion.subject_id),
        markdown_safe(&assertion.summary)
    )
    .expect("write to string");
}

/// The Gap register as a bounded, grouped artifact plus its JSON sidecar
/// (#240, AC-0210). Past [`GAP_REGISTER_FLAT_LIMIT`] rows the Markdown is
/// grouped by cause class with at most [`GAP_CLASS_REPRESENTATIVES`]
/// instances per class and [`GAP_REGISTER_MAX_CLASSES`] classes, and every
/// omission is an explicit counted line — nothing is silently dropped. The
/// returned assertions stay complete: the Gaps lane groups them and each
/// keeps its inline provenance in the bundle. The sidecar indexes every
/// instance by class.
fn gap_register(
    nodes: &[&Node],
    edges: &[&Edge],
    flow_assertions: &[SpecAssertion],
    findings: usize,
    mode: ExportMode,
) -> (String, String, Vec<SpecAssertion>) {
    let mut assertions: Vec<SpecAssertion> = nodes
        .iter()
        .filter(|node| is_gap_node(node))
        .map(|node| node_assertion(node, gap_node_edge_label(node, edges)))
        .collect();
    assertions.extend(
        edges
            .iter()
            .filter(|edge| is_gap_edge(edge))
            .map(|edge| edge_assertion(edge)),
    );
    assertions.extend(
        flow_assertions
            .iter()
            .filter(|assertion| assertion.provenance.confidence_tier == ConfidenceTier::Gap)
            .cloned(),
    );
    assertions.sort_by(|left, right| left.id.cmp(&right.id));
    assertions.dedup_by(|left, right| left.id == right.id);
    let (content, sidecar) = render_gap_register(&assertions, findings, mode);
    (content, sidecar, assertions)
}

/// Split register rows the way the Gaps lane does (#241): a Gap node is a
/// finding; a Gap edge touching a listed Gap node supports that node's
/// finding instead of doubling it, while an edge-only Gap (no Gap node on
/// either end) stays a finding of its own; flow-hop assertions restate gaps
/// already listed. Classes are built from the findings alone so the Markdown
/// and the lane agree on classes and counts.
fn lane_findings(assertions: &[SpecAssertion]) -> (Vec<SpecAssertion>, Vec<&str>) {
    let listed: std::collections::BTreeSet<&str> = assertions
        .iter()
        .filter(|assertion| assertion.id.starts_with("node:"))
        .map(|assertion| assertion.subject_id.as_str())
        .collect();
    let supports_listed = |assertion: &SpecAssertion| {
        let mut parts = assertion.subject_id.split(' ');
        let src = parts.next().unwrap_or_default();
        let dst = parts.nth(1).unwrap_or_default();
        listed.contains(src) || listed.contains(dst)
    };
    let mut findings = Vec::new();
    let mut supporting = Vec::new();
    for assertion in assertions {
        let is_finding = assertion.id.starts_with("node:")
            || (assertion.id.starts_with("edge:") && !supports_listed(assertion));
        if is_finding {
            findings.push(assertion.clone());
        } else {
            supporting.push(assertion.id.as_str());
        }
    }
    (findings, supporting)
}

/// Render the register prose and its JSON index from the sorted assertions.
fn render_gap_register(
    assertions: &[SpecAssertion],
    findings: usize,
    mode: ExportMode,
) -> (String, String) {
    let (lane_rows, supporting) = lane_findings(assertions);
    let classes = gap_classes(&lane_rows);

    let mut content = String::from("# Gap register\n\n");
    if assertions.len() <= GAP_REGISTER_FLAT_LIMIT {
        content.push_str("| Subject | Reason | Omission class | Filled by |\n|---|---|---|---|\n");
        for assertion in assertions {
            register_row(&mut content, assertion);
        }
        if assertions.is_empty() {
            content.push_str("| — | No unresolved facts | — | — |\n");
        }
        render_omission_rollup(&mut content, &omission_rollup(&lane_rows));
        append_assertions(&mut content, assertions);
    } else {
        let shown = &classes[..classes.len().min(GAP_REGISTER_MAX_CLASSES)];
        writeln!(
            content,
            "{findings} open findings · {} register rows: {} findings in {} cause \
             classes (stop reason × extractor), largest first, plus {} supporting \
             rows that restate a listed gap. Each class lists up to \
             {GAP_CLASS_REPRESENTATIVES} representative instances; \
             `gap_register.json` lists every instance by class, and each \
             instance keeps its inline provenance in the bundle's assertions.\n",
            assertions.len(),
            lane_rows.len(),
            classes.len(),
            supporting.len(),
        )
        .expect("write to string");
        content.push_str(
            "| Class | Cause | Extractor | Next tier | Omission class | Filled by | Instances |\n|---|---|---|---|---|---|---|\n",
        );
        for (index, class) in shown.iter().enumerate() {
            let (omission, seam) = uniform_omission(&class.members)
                .map_or(("mixed", "mixed"), |omission| {
                    omission_cells(Some(omission))
                });
            writeln!(
                content,
                "| {} | {} | `{}` | {} | {omission} | {seam} | {} |",
                class_id(index),
                markdown_safe(&short_cause(&class.cause)),
                markdown_safe(class.extractor),
                class.next_tier,
                class.members.len(),
            )
            .expect("write to string");
        }
        let hidden = &classes[shown.len()..];
        if !hidden.is_empty() {
            writeln!(
                content,
                "| … | {} more classes, not detailed here — listed in `gap_register.json` | — | — | — | — | {} |",
                hidden.len(),
                hidden.iter().map(|class| class.members.len()).sum::<usize>(),
            )
            .expect("write to string");
        }
        render_omission_rollup(&mut content, &omission_rollup(&lane_rows));
        let mut representatives: Vec<SpecAssertion> = Vec::new();
        for (index, class) in shown.iter().enumerate() {
            writeln!(
                content,
                "\n## {} — {} ({} instances)\n\n| Subject | Reason | Omission class | Filled by |\n|---|---|---|---|",
                class_id(index),
                markdown_safe(&short_cause(&class.cause)),
                class.members.len(),
            )
            .expect("write to string");
            for member in class.members.iter().take(GAP_CLASS_REPRESENTATIVES) {
                register_row(&mut content, member);
                representatives.push((*member).clone());
            }
            let more = class
                .members
                .len()
                .saturating_sub(GAP_CLASS_REPRESENTATIVES);
            if more > 0 {
                writeln!(
                    content,
                    "\n… {more} more instances in this class — listed in `gap_register.json` under `{}`.",
                    class_id(index),
                )
                .expect("write to string");
            }
        }
        append_assertions(&mut content, &representatives);
        writeln!(
            content,
            "\nInline provenance is shown for the {} representative instances above; \
             the other {} carry theirs in the bundle's structured assertions.",
            representatives.len(),
            assertions.len() - representatives.len(),
        )
        .expect("write to string");
    }

    let index = GapRegisterIndex {
        schema: "cartograph.gap-register-index/v1",
        mode,
        findings,
        instances: assertions.len(),
        supporting,
        by_omission: omission_rollup(&lane_rows),
        classes: classes
            .iter()
            .enumerate()
            .map(|(index, class)| GapClassIndex {
                id: class_id(index),
                cause: &class.cause,
                extractor: class.extractor,
                next_tier: class.next_tier,
                omission_class: uniform_omission(&class.members).map(|omission| omission.class),
                seam: uniform_omission(&class.members).map(|omission| omission.seam),
                instances: class.members.len(),
                members: class
                    .members
                    .iter()
                    .map(|member| member.id.as_str())
                    .collect(),
            })
            .collect(),
    };
    let mut sidecar = serde_json::to_string(&index).expect("serialize gap register index");
    sidecar.push('\n');
    (content, sidecar)
}

/// True when `node` records ADR/code drift (see [`is_gap_node`] for why
/// these predicates are public).
pub fn is_drift_node(node: &Node) -> bool {
    node.label == "Drift" || node.props["kind"].as_str() == Some("drift")
}

fn drift_register(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>, usize) {
    let drift_nodes: Vec<&&Node> = nodes.iter().filter(|node| is_drift_node(node)).collect();
    let mut assertions: Vec<SpecAssertion> = drift_nodes
        .iter()
        .map(|node| node_assertion(node, None))
        .collect();
    assertions.extend(
        edges
            .iter()
            .filter(|edge| is_drift_edge(edge))
            .map(|edge| edge_assertion(edge)),
    );
    let mut content = String::from(
        "# Drift register\n\n| Finding | ADR | Offending edge | Flow triggers | Confidence |\n|---|---|---|---|---|\n",
    );
    for node in &drift_nodes {
        let triggers = node.props["flow_triggers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let node_provenance = provenance(&node.props, &node.id);
        writeln!(
            content,
            "| {} | `{}` | `{}` | {} | {:?} |",
            markdown_safe(&node_name(node)),
            markdown_safe(node.props["adr_id"].as_str().unwrap_or("—")),
            markdown_safe(node.props["offending_edge"].as_str().unwrap_or("—")),
            markdown_safe(if triggers.is_empty() {
                "—"
            } else {
                &triggers
            }),
            node_provenance.confidence_tier,
        )
        .expect("write to string");
    }
    if drift_nodes.is_empty() {
        content.push_str("| — | No ADR/code conflicts recovered |\n");
    }
    (content, assertions, drift_nodes.len())
}

fn security_view(nodes: &[&Node]) -> (String, Vec<SpecAssertion>, usize) {
    let findings = nodes
        .iter()
        .filter(|node| node.label == "Finding" && node.props["kind"].as_str() == Some("security"))
        .collect::<Vec<_>>();
    let assertions = findings
        .iter()
        .map(|finding| node_assertion(finding, None))
        .collect::<Vec<_>>();
    let mut content = String::from(
        "# Security findings\n\n| Finding | Type | Subject | Resource scope | Actions | US / AC | Confidence |\n|---|---|---|---|---|---|---|\n",
    );
    for finding in &findings {
        let scopes = finding.props["resource_scope"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let actions = finding.props["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let finding_provenance = provenance(&finding.props, &finding.id);
        writeln!(
            content,
            "| {} | `{}` | `{}` | {} | {} | {} / {} | {:?} |",
            markdown_safe(&node_name(finding)),
            markdown_safe(finding.props["category"].as_str().unwrap_or("security")),
            markdown_safe(finding.props["subject_id"].as_str().unwrap_or("—")),
            markdown_safe(if scopes.is_empty() { "—" } else { &scopes }),
            markdown_safe(if actions.is_empty() { "—" } else { &actions }),
            markdown_safe(finding.props["us_id"].as_str().unwrap_or("US-0015")),
            markdown_safe(finding.props["ac_id"].as_str().unwrap_or("—")),
            finding_provenance.confidence_tier,
        )
        .expect("write to string");
    }
    if findings.is_empty() {
        content.push_str("| — | — | — | — | — | No explicit security findings | — |\n");
    }
    (content, assertions, findings.len())
}

/// The toolchain view (#215): what the system is built *with*, as cited
/// facts — `Tool` nodes with their resolved settings and the config files
/// (`DEFINED_IN`) that prove them.
fn toolchain_view(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let mut content = String::from(
        "# Toolchain\n\n| Tool | Category | Defined in | Settings | Confidence |\n|---|---|---|---|---|\n",
    );
    let mut assertions = Vec::new();
    let tools: Vec<&&Node> = nodes.iter().filter(|node| node.label == "Tool").collect();
    for tool in &tools {
        let tool_provenance = provenance(&tool.props, &tool.id);
        let display = tool.props["display"].as_str().unwrap_or(&tool.id);
        let category = tool.props["category"].as_str().unwrap_or("—");
        let mut defined_in: Vec<String> = edges
            .iter()
            .filter(|edge| edge.label == "DEFINED_IN" && edge.src == tool.id)
            .map(|edge| {
                edge.dst
                    .split_once('@')
                    .map(|(_, path)| path.to_string())
                    .unwrap_or_else(|| edge.dst.clone())
            })
            .collect();
        defined_in.sort();
        defined_in.dedup();
        let settings = if tool.props["settings_behind_code"].as_bool() == Some(true) {
            "settings live in code — detected by presence only".to_string()
        } else {
            let rendered: Vec<String> = tool.props["settings"]
                .as_object()
                .map(|settings| {
                    settings
                        .iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect()
                })
                .unwrap_or_default();
            if rendered.is_empty() {
                "—".to_string()
            } else {
                rendered.join("; ")
            }
        };
        writeln!(
            content,
            "| {} | {} | {} | {} | {:?} |",
            markdown_safe(display),
            markdown_safe(category),
            markdown_safe(&defined_in.join(", ")),
            markdown_safe(&settings),
            tool_provenance.confidence_tier,
        )
        .expect("write to string");
        assertions.push(SpecAssertion {
            id: format!("tool:{}", tool.id),
            subject_id: tool.id.clone(),
            subject_kind: "Tool".into(),
            summary: format!(
                "Toolchain: {display} ({category}) defined in {}",
                defined_in.join(", ")
            ),
            omission: gap_omission(&tool_provenance, &tool.props, "toolchain fact unresolved"),
            provenance: tool_provenance,
            edge_label: None,
        });
    }
    if tools.is_empty() {
        content.push_str("| — | — | — | No toolchain facts recovered | — |\n");
    }
    (content, assertions)
}

/// Compile the complete official artifact set with one R-INT-5 policy.
/// Rejected inferred content hashes are suppressed without upgrading any fact.
pub fn compile_spec(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> SpecBundle {
    // Curation is part of the compilation projection, so derived ADRs may
    // only consume facts that survived the selected export policy. Otherwise
    // a rejected support edge could disappear while a new derived hash kept
    // its conclusion visible.
    let base_nodes = filter_nodes(nodes, mode, rejected_hashes)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    // A filtered rule must not survive through a separately confirmed
    // relationship. Keep every artifact on the same rule projection.
    let all_rule_ids: BTreeSet<_> = nodes
        .iter()
        .filter(|node| node.label == "BusinessRule")
        .map(|node| node.id.as_str())
        .collect();
    let visible_node_ids: BTreeSet<_> = base_nodes.iter().map(|node| node.id.as_str()).collect();
    let base_edges = filter_edges(edges, mode, rejected_hashes)
        .into_iter()
        .filter(|edge| {
            let touches_rule = all_rule_ids.contains(edge.src.as_str())
                || all_rule_ids.contains(edge.dst.as_str());
            !touches_rule
                || (visible_node_ids.contains(edge.src.as_str())
                    && visible_node_ids.contains(edge.dst.as_str()))
        })
        .cloned()
        .collect::<Vec<_>>();
    let projected_flows = project_flows(flows, mode, rejected_hashes);
    let derived = crate::derive_adr_facts(&base_nodes, &base_edges, &projected_flows);
    let security_findings = crate::security::derive_security_findings(&base_nodes, &base_edges);
    let mut projected_nodes = base_nodes;
    projected_nodes.extend(derived.nodes);
    projected_nodes.extend(security_findings);
    let mut projected_edges = base_edges;
    projected_edges.extend(derived.edges);
    let nodes = filter_nodes(&projected_nodes, mode, rejected_hashes);
    let edges = filter_edges(&projected_edges, mode, rejected_hashes);
    let (stories, story_assertions) = recovered_user_stories(&nodes);
    let (matrix, matrix_index, matrix_assertions) = traceability_matrix(&edges, mode);
    let (dossiers, flow_assertions) = flow_artifact(flows, mode, rejected_hashes);
    let (topology, topology_assertions) = topology_artifact(&nodes, &edges);
    let (data, data_assertions) = data_model(&nodes, &edges);
    let (adrs, adr_assertions) = adr_set(&nodes, &edges);
    let (drifts, drift_assertions, drift_count) = drift_register(&nodes, &edges);
    let (security, security_assertions, security_count) = security_view(&nodes);
    let (toolchain, toolchain_assertions) = toolchain_view(&nodes, &edges);
    let (rules, rule_index, rule_assertions) = crate::rules::inventory(&nodes, &edges, mode);

    // The register still lists supporting edge and flow-hop assertions as
    // rows, but the count the Workbench displays uses the shared finding
    // definition, so it reconciles with the findings-summary headline.
    let gap_count = count_gap_findings(nodes.iter().copied(), edges.iter().copied());
    let (gaps, gap_index, gap_assertions) =
        gap_register(&nodes, &edges, &flow_assertions, gap_count, mode);
    let artifacts = vec![
        artifact(
            "user-stories",
            "user_stories.md",
            "User stories",
            "markdown",
            stories,
            story_assertions,
        ),
        // The matrix renders its own bounded provenance table (#487).
        identified_artifact(
            "us-tm",
            "US-TM.md",
            "US traceability matrix",
            "markdown",
            matrix,
            matrix_assertions,
        ),
        // Every matrix link by class; the assertions stay on the matrix.
        identified_artifact(
            "us-tm-index",
            "US-TM.json",
            "US traceability matrix index",
            "json",
            matrix_index,
            Vec::new(),
        ),
        artifact(
            "flow-dossiers",
            "flow_dossiers.md",
            "Flow dossiers",
            "markdown",
            dossiers,
            flow_assertions,
        ),
        artifact(
            "topology",
            "topology.md",
            "Resource topology",
            "markdown",
            topology,
            topology_assertions,
        ),
        artifact(
            "data-model",
            "data_model.md",
            "Data model",
            "markdown",
            data,
            data_assertions,
        ),
        artifact(
            "adrs",
            "adrs.md",
            "Architecture decisions",
            "markdown",
            adrs,
            adr_assertions,
        ),
        // The register renders its own bounded provenance table (#240).
        identified_artifact(
            "gap-register",
            "gap_register.md",
            "Gap register",
            "markdown",
            gaps,
            gap_assertions,
        ),
        // Every register instance by class; the assertions stay on the
        // register above so they are counted once.
        identified_artifact(
            "gap-register-index",
            "gap_register.json",
            "Gap register index",
            "json",
            gap_index,
            Vec::new(),
        ),
        artifact(
            "drift-register",
            "drift_register.md",
            "Drift register",
            "markdown",
            drifts,
            drift_assertions,
        ),
        artifact(
            "security-view",
            "security.md",
            "Security findings",
            "markdown",
            security,
            security_assertions,
        ),
        artifact(
            "toolchain",
            "toolchain.md",
            "Toolchain",
            "markdown",
            toolchain,
            toolchain_assertions,
        ),
        // The inventory renders its own bounded provenance table (#487).
        identified_artifact(
            "rule-evidence",
            "rule-evidence.md",
            "Source rule evidence",
            "markdown",
            rules,
            rule_assertions,
        ),
        // Every observation and relationship by source file.
        identified_artifact(
            "rule-evidence-index",
            "rule-evidence.json",
            "Source rule evidence index",
            "json",
            rule_index,
            Vec::new(),
        ),
    ];
    let assertion_count = artifacts
        .iter()
        .map(|artifact| artifact.assertions.len())
        .sum();
    SpecBundle {
        mode,
        artifacts,
        assertion_count,
        gap_count,
        drift_count,
        security_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                "rule-evidence.md",
                "rule-evidence.json",
            ]
        );
        for artifact in &bundle.artifacts {
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
                .contains("Unauthenticated endpoint: GET /admin")
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
            "| C-01 | computed identity | `spec.workbench.test` | T1 | unresolved relationship | interpretation | 30 |"
        ));
        // AC-0254 (#585): the register groups findings by omission class and
        // names the seam that would fill each.
        assert!(content.contains(
            "## By omission class\n\nWhy each finding is missing, and the seam that would fill it.\n\n\
             | Omission class | Filled by | Findings |\n|---|---|---|\n\
             | missing input | manifest input | 8 |\n\
             | unresolved relationship | adapter | 7 |\n\
             | unresolved relationship | interpretation | 31 |\n"
        ));
        assert_eq!(
            index["by_omission"],
            serde_json::json!([
                {"class": "missing_input", "seam": "manifest_input", "findings": 8},
                {"class": "unresolved_relationship", "seam": "adapter", "findings": 7},
                {"class": "unresolved_relationship", "seam": "interpretation", "findings": 31},
            ])
        );
        assert_eq!(classes[1]["omission_class"], "missing_input");
        assert_eq!(classes[1]["seam"], "manifest_input");
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
                    omission.class.label(),
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
}
