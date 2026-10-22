//! Cross-layer flow tracer (SPEC-00 §5, US-0006) — the deterministic (T0)
//! path engine.
//!
//! A flow starts at a trigger — a `Screen` (M4, the user-action anchor),
//! an extension user-action anchor (`ExtensionContext` or manifest
//! `Command`, #165), an `Endpoint` nothing local fetches, or a `Channel`
//! no local producer publishes to — and walks the graph hop by hop:
//! `RENDERS` into components, `FETCHES` into endpoints, `HANDLES` into
//! the handler symbol or context, `ENTRY` into a context's entry file,
//! `DEFINED_IN` in reverse from a file into the symbols it defines,
//! `CALLS` through the call graph, `PUBLISHES` onto channels,
//! `SUBSCRIBES` out to consumers. Each hop records the tier and confidence
//! of the edge that resolved it (AC-0015). A hop into a `Gap` node
//! truncates that branch — the flow is emitted partial, never silently
//! completed (AC-0016, R-INT-4). A flow node that `DEPENDS_ON` an
//! execution Gap (code it runs that T0 could not recover, such as an
//! unproven `eval()`) contributes that Gap as a hop too (AC-0219,
//! ADR-0032). Completed traces score per §5.3
//! (AC-0017). At M3 every resolvable hop is T0; the T1–T3 rungs join the
//! ladder at M6–M8.

use core_graph::{Edge, Node};
use core_prov::Provenance;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

mod bounded;
use bounded::{Candidate, FlowView, HopView, TraceBudget};
pub use bounded::{
    TRACE_WORKSPACE_MODEL, TraceGraphBound, TraceLimit, TraceLimitExceeded, TraceLimits,
    TraceWorkspaceBound, logical_workspace_bound,
};

/// Edge labels the tracer walks — callers use this to query exactly the
/// edges a trace needs.
pub const FLOW_EDGE_LABELS: &[&str] = &[
    "RENDERS",
    "FETCHES",
    "HANDLES",
    "ENTRY",
    "DEFINED_IN",
    "CALLS",
    "PUBLISHES",
    "SUBSCRIBES",
    // Walked only into execution Gaps (`owns_execution_gap`), never onward.
    "DEPENDS_ON",
];

/// Edge labels the tracer walks against their stored direction:
/// `SUBSCRIBES` is stored consumer → channel and `DEFINED_IN` symbol → file,
/// but hops run channel → consumer and entry file → symbol.
pub const REVERSED_FLOW_LABELS: &[&str] = &["DEFINED_IN", "SUBSCRIBES"];

/// Node labels the tracer needs (for classification and display names).
pub const FLOW_NODE_LABELS: &[&str] = &[
    "Screen",
    "Component",
    "Endpoint",
    "Symbol",
    "Channel",
    "ExtensionContext",
    "Command",
    "Gap",
    "File",
];

/// Traversal bound (SPEC-00 US-0006 performance note: path queries bounded).
const MAX_DEPTH: usize = 64;

/// Flow confidence status per SPEC-00 §5.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum FlowStatus {
    /// Every hop Confirmed.
    Verified,
    /// At least one Gap hop — emitted with the gap explicit.
    Partial,
    /// No gaps, but at least one inferred hop.
    Inferred,
}

impl FlowStatus {
    /// Display form used by artifacts.
    pub fn as_str(self) -> &'static str {
        match self {
            FlowStatus::Verified => "Verified",
            FlowStatus::Partial => "Partial",
            FlowStatus::Inferred => "Inferred",
        }
    }
}

/// One resolved hop of a flow.
#[derive(Debug, Clone, Serialize)]
pub struct Hop {
    /// Edge label (`HANDLES`, `CALLS`, `PUBLISHES`, `SUBSCRIBES`).
    pub label: String,
    /// Hop source node id (traversal direction, not storage direction:
    /// `SUBSCRIBES` hops run channel → consumer).
    pub src: String,
    /// Hop target node id.
    pub dst: String,
    /// Display name of the source node.
    pub src_name: String,
    /// Display name of the target node.
    pub dst_name: String,
    /// Tier that resolved this hop (from the edge's provenance, AC-0015).
    pub tier: String,
    /// Confidence tier of the hop (R-INT-2: always visible).
    pub confidence: String,
    /// First evidence ref as `path bytes start..end`, if present.
    pub evidence: Option<String>,
    /// Complete edge provenance for inline Workbench/export rendering.
    pub provenance: Provenance,
    /// Gap reason copied from the destination Gap node, when this hop is
    /// unresolved. Kept structured so UI/export surfaces never parse a label.
    pub gap_reason: Option<String>,
    /// Escalation tiers already attempted before the explicit Gap was emitted.
    pub attempted_tiers: Vec<String>,
}

impl Hop {
    /// The stored edge this hop traverses, as `(src, label, dst)`: the hop's
    /// own endpoints, swapped back for a label in `REVERSED_FLOW_LABELS`.
    pub fn stored_edge(&self) -> (&str, &str, &str) {
        if REVERSED_FLOW_LABELS.contains(&self.label.as_str()) {
            (&self.dst, &self.label, &self.src)
        } else {
            (&self.src, &self.label, &self.dst)
        }
    }
}

/// One traced flow: a trigger and every hop reachable from it.
#[derive(Debug, Clone, Serialize)]
pub struct Flow {
    /// Trigger node id.
    pub trigger: String,
    /// Trigger node label (`Screen`, `ExtensionContext`, `Command`,
    /// `Endpoint`, or `Channel`).
    pub trigger_kind: String,
    /// Trigger display name.
    pub trigger_name: String,
    /// Hops in traversal order (deduplicated).
    pub hops: Vec<Hop>,
    /// Status per §5.3.
    pub status: FlowStatus,
    /// Mean hop weight per §5.3 (Confirmed 1.0 … Gap 0.0).
    pub score: f64,
    /// True when the traversal depth bound cut the walk short — downstream
    /// hops exist that are not in `hops`. Forces `Partial` (R-INT-4: a
    /// bounded trace is an unresolved continuation, never a silent finish).
    pub depth_limited: bool,
}

fn hop_weight(confidence: &str) -> f64 {
    match confidence {
        "Confirmed" => 1.0,
        "InferredStrong" => 0.6,
        "InferredWeak" => 0.3,
        _ => 0.0,
    }
}

/// True when `edge` is a `DEPENDS_ON` from its owner into a Gap that stands
/// for code the owner runs but T0 could not recover (AC-0219, ADR-0032):
/// an unproven `eval()` / `new Function()` argument, or any future Gap an
/// adapter hangs off its owner this way — fail closed, so an unknown kind
/// counts. A Gap flagged `rule_evidence_gap` is excluded: it scopes where
/// source-rule *evidence* stops (including decoded eval code awaiting a
/// span mapping), while the owner's calls were still recovered. Every other
/// `DEPENDS_ON` (e.g. Terraform ordering between resources) is not a flow
/// hop at all.
fn owns_execution_gap(edge: &Edge, by_id: &BTreeMap<&str, &Node>) -> bool {
    let node = by_id.get(edge.dst.as_str());
    let is_gap = node.map_or(edge.dst.starts_with("gap:"), |node| node.label == "Gap");
    let rule_scoped = |props: &serde_json::Value| props["rule_evidence_gap"] == true;
    is_gap && !rule_scoped(&edge.props) && !node.is_some_and(|node| rule_scoped(&node.props))
}

/// Trace every flow in the graph slice. Deterministic: triggers in sorted
/// id order, hops in traversal order with sorted expansion (US-0014).
pub fn trace(nodes: &[Node], edges: &[Edge]) -> Vec<Flow> {
    trace_impl(nodes, edges, None).expect("unlimited tracing")
}

/// Trace complete flows under cumulative output limits, failing explicitly
/// instead of returning a truncated result. Bytes are the exact compact
/// UTF-8 JSON serialization of the returned `Vec<Flow>`, including brackets
/// and commas. Counts cover all triggers and sum the deduplicated hops of each
/// flow; a hop shared by multiple flows counts once in each returned flow.
///
/// The caller must separately admit the input graph. Borrowed adjacency,
/// traversal descriptors and provenance views use linear input workspace;
/// owned flow/hop strings and provenance are created only after admission.
pub fn trace_bounded(
    nodes: &[Node],
    edges: &[Edge],
    limits: TraceLimits,
) -> Result<Vec<Flow>, TraceLimitExceeded> {
    trace_impl(nodes, edges, Some(limits))
}

fn trace_impl(
    nodes: &[Node],
    edges: &[Edge],
    limits: Option<TraceLimits>,
) -> Result<Vec<Flow>, TraceLimitExceeded> {
    let mut budget = TraceBudget::new(limits)?;
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // Adjacency, sorted for deterministic expansion. SUBSCRIBES is stored
    // consumer → channel and DEFINED_IN symbol → file; the tracer walks
    // them reversed (channel → consumer, entry file → its symbols).
    let mut out_edges: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
    let mut chan_consumers: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
    let mut file_symbols: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
    let mut published_to: HashSet<&str> = HashSet::new();
    for edge in edges {
        match edge.label.as_str() {
            "RENDERS" | "FETCHES" | "HANDLES" | "ENTRY" | "CALLS" | "PUBLISHES" => {
                out_edges.entry(edge.src.as_str()).or_default().push(edge);
                if edge.label == "PUBLISHES" {
                    published_to.insert(edge.dst.as_str());
                }
            }
            "SUBSCRIBES" => {
                chan_consumers
                    .entry(edge.dst.as_str())
                    .or_default()
                    .push(edge);
            }
            "DEPENDS_ON" if owns_execution_gap(edge, &by_id) => {
                out_edges.entry(edge.src.as_str()).or_default().push(edge);
            }
            "DEFINED_IN" => {
                file_symbols
                    .entry(edge.dst.as_str())
                    .or_default()
                    .push(edge);
            }
            _ => {}
        }
    }
    for v in out_edges.values_mut() {
        v.sort_by(|a, b| (&a.label, &a.dst).cmp(&(&b.label, &b.dst)));
    }
    for v in chan_consumers.values_mut() {
        v.sort_by(|a, b| a.src.cmp(&b.src));
    }
    for v in file_symbols.values_mut() {
        v.sort_by(|a, b| a.src.cmp(&b.src));
    }

    // Triggers (SPEC-00 §5.1): every user-action anchor — a Screen, an
    // ExtensionContext (popup, content script, background), or a manifest
    // Command (#165) — is traced first; an endpoint keeps its own flow
    // unless an anchor's traversal actually *reached* it (a FETCHES edge
    // from an unrendered component must not make the server flow
    // disappear); channels nothing local publishes to are external events
    // entering the slice.
    let mut anchors: Vec<&Node> = nodes
        .iter()
        .filter(|n| matches!(n.label.as_str(), "Screen" | "ExtensionContext" | "Command"))
        .collect();
    anchors.sort_by(|a, b| a.id.cmp(&b.id));
    let mut flows = Vec::new();
    for trigger in anchors {
        flows.push(trace_one(
            trigger,
            &by_id,
            &out_edges,
            &chan_consumers,
            &file_symbols,
            &mut budget,
        )?);
    }
    let covered: HashSet<&str> = flows
        .iter()
        .flat_map(|f| f.hops.iter())
        .filter(|h| h.label == "FETCHES")
        .map(|h| h.dst.as_str())
        .collect();

    let mut rest: Vec<&Node> = nodes
        .iter()
        .filter(|n| n.label == "Endpoint" && !covered.contains(n.id.as_str()))
        .collect();
    rest.extend(nodes.iter().filter(|n| {
        n.label == "Channel"
            && !published_to.contains(n.id.as_str())
            && chan_consumers.contains_key(n.id.as_str())
    }));
    rest.sort_by(|a, b| a.id.cmp(&b.id));
    rest.dedup_by(|a, b| a.id == b.id);
    for trigger in rest {
        flows.push(trace_one(
            trigger,
            &by_id,
            &out_edges,
            &chan_consumers,
            &file_symbols,
            &mut budget,
        )?);
    }
    flows.sort_by(|a, b| a.trigger.cmp(&b.trigger));
    Ok(flows)
}

/// One anchor kind the tracer sought and how many the slice actually holds
/// (#165) — zero flows must name what was looked for, never shrug.
#[derive(Debug, Clone, Serialize)]
pub struct AnchorProbe {
    /// Human-readable anchor kind, stable and display-ready.
    pub kind: String,
    /// Nodes of that kind present in the traced slice.
    pub found: usize,
}

/// Count every anchor kind `trace` uses, in the order it seeks them.
/// Deterministic for a given slice; independent of input order.
pub fn anchor_probes(nodes: &[Node], edges: &[Edge]) -> Vec<AnchorProbe> {
    let mut published_to: HashSet<&str> = HashSet::new();
    let mut consumed: HashSet<&str> = HashSet::new();
    for edge in edges {
        match edge.label.as_str() {
            "PUBLISHES" => {
                published_to.insert(edge.dst.as_str());
            }
            "SUBSCRIBES" => {
                consumed.insert(edge.dst.as_str());
            }
            _ => {}
        }
    }
    let count = |label: &str| nodes.iter().filter(|n| n.label == label).count();
    let external_channels = nodes
        .iter()
        .filter(|n| {
            n.label == "Channel"
                && consumed.contains(n.id.as_str())
                && !published_to.contains(n.id.as_str())
        })
        .count();
    vec![
        AnchorProbe {
            kind: "screens (routes/pages)".into(),
            found: count("Screen"),
        },
        AnchorProbe {
            kind: "extension contexts (popup, background, content scripts)".into(),
            found: count("ExtensionContext"),
        },
        AnchorProbe {
            kind: "extension commands (keyboard shortcuts)".into(),
            found: count("Command"),
        },
        AnchorProbe {
            kind: "HTTP endpoints".into(),
            found: count("Endpoint"),
        },
        AnchorProbe {
            kind: "externally published event channels".into(),
            found: external_channels,
        },
    ]
}

fn trace_one<'a>(
    trigger: &'a Node,
    by_id: &BTreeMap<&'a str, &'a Node>,
    out_edges: &BTreeMap<&'a str, Vec<&'a Edge>>,
    chan_consumers: &BTreeMap<&'a str, Vec<&'a Edge>>,
    file_symbols: &BTreeMap<&'a str, Vec<&'a Edge>>,
    budget: &mut TraceBudget,
) -> Result<Flow, TraceLimitExceeded> {
    budget.start_flow()?;
    let mut hops = Vec::new();
    let mut seen_hops = HashSet::new();
    let mut visited = HashSet::new();
    let mut stack = vec![(trigger.id.as_str(), 0)];
    let mut depth_limited = false;
    let mut has_gap = false;
    let mut all_confirmed = true;
    let mut weight = 0.0;

    while let Some((id, depth)) = stack.pop() {
        if depth >= MAX_DEPTH {
            if out_edges.contains_key(id)
                || chan_consumers.contains_key(id)
                || file_symbols.contains_key(id)
            {
                depth_limited = true;
            }
            continue;
        }
        if !visited.insert(id) {
            continue;
        }
        if by_id.get(id).is_some_and(|node| node.label == "Gap") {
            continue;
        }

        // Only graph references enter this transient fan-out. No candidate
        // owns names, provenance, identities or destination strings.
        let next: Vec<_> = out_edges
            .get(id)
            .into_iter()
            .flatten()
            .map(|edge| Candidate {
                edge,
                reversed: false,
            })
            .chain(
                chan_consumers
                    .get(id)
                    .into_iter()
                    .flatten()
                    .chain(file_symbols.get(id).into_iter().flatten())
                    .map(|edge| Candidate {
                        edge,
                        reversed: true,
                    }),
            )
            .collect();
        for candidate in &next {
            let key = (
                candidate.edge.label.as_str(),
                candidate.src(),
                candidate.dst(),
            );
            if seen_hops.insert(key) {
                budget.admit_hop()?;
                let view = HopView::new(*candidate, by_id);
                has_gap |= view.confidence == "Gap"
                    || by_id
                        .get(candidate.dst())
                        .is_some_and(|node| node.label == "Gap");
                all_confirmed &= view.confidence == "Confirmed";
                weight += hop_weight(view.confidence);
                hops.push(*candidate);
            }
        }
        for candidate in next.into_iter().rev() {
            stack.push((candidate.dst(), depth + 1));
        }
    }

    let status = if has_gap || hops.is_empty() || depth_limited {
        FlowStatus::Partial
    } else if all_confirmed {
        FlowStatus::Verified
    } else {
        FlowStatus::Inferred
    };
    let score = if hops.is_empty() {
        0.0
    } else {
        weight / hops.len() as f64
    };
    let view = FlowView::new(trigger, &hops, by_id, status, score, depth_limited);
    budget.admit_flow(&view)?;
    Ok(view.into_owned())
}

#[cfg(test)]
mod tests;
