//! Pure deterministic request planning; no source or store access.

use agents::AgentCandidate;
use core_graph::source::{FactKey, edge_digest, node_digest};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::BTreeSet;

pub(super) const MAX_REQUESTS: usize = 64;
pub(super) const MAX_CANDIDATES: usize = 8;

pub(super) struct EvidenceRequest {
    pub fact: FactKey,
    pub fact_digest: String,
    pub reference: Option<EvidenceRef>,
    pub candidate: Option<AgentCandidate>,
}

pub(super) struct TaskPlan {
    pub action_id: String,
    pub gap_id: String,
    pub source_id: String,
    pub edge_label: String,
    pub requests: Vec<EvidenceRequest>,
    pub slot: Option<(FactKey, String)>,
    pub unplanned_candidates: usize,
}

fn request(node: &Node, candidate: bool) -> Result<EvidenceRequest, String> {
    let provenance = spec::provenance(&node.props, &node.id);
    let name = node.props["name"]
        .as_str()
        .or_else(|| node.props["path"].as_str())
        .unwrap_or(&node.id);
    Ok(EvidenceRequest {
        fact: FactKey::from_node(node),
        fact_digest: node_digest(node).map_err(|_| "Task fact is invalid.")?,
        reference: provenance.evidence.first().cloned(),
        candidate: candidate.then(|| AgentCandidate {
            node_id: node.id.clone(),
            label: node.label.clone(),
            summary: format!("{}: {name}", node.label),
            evidence_ids: Vec::new(),
        }),
    })
}

pub(super) fn plan(
    nodes: &[Node],
    edges: &[Edge],
    gap_id: &str,
    action_id: &str,
    context_hops: u32,
) -> Result<TaskPlan, String> {
    let gap = nodes
        .iter()
        .find(|node| node.id == gap_id && spec::is_gap_node(node))
        .ok_or("The selected gap is unavailable; refresh recovery.")?;
    // Preserve the existing adjacent-slot heuristic. This is not a proof of
    // direction/cardinality and does not extend the broker's relation allowlist.
    let slot = edges
        .iter()
        .find(|edge| edge.dst == gap_id)
        .or_else(|| edges.iter().find(|edge| edge.src == gap_id));
    let source_id = slot
        .map(|edge| {
            if edge.dst == gap_id {
                &edge.src
            } else {
                &edge.dst
            }
        })
        .map(String::as_str)
        .or_else(|| gap.props["source_id"].as_str())
        .ok_or("The selected gap has no source fact.")?;
    let source = nodes
        .iter()
        .find(|node| node.id == source_id)
        .ok_or("The selected gap's source fact is unavailable.")?;
    let edge_label = slot
        .map(|edge| edge.label.as_str())
        .or_else(|| gap.props["edge_label"].as_str())
        .unwrap_or("CALLS");
    let subgraph = semantic::context::khop_subgraph(nodes, edges, gap_id, context_hops);
    let in_context: BTreeSet<_> = subgraph
        .nodes
        .iter()
        .map(|(_, node)| node.id.as_str())
        .collect();
    let mut requests = vec![request(source, false)?, request(gap, false)?];
    let mut unplanned_candidates = 0usize;
    for node in nodes {
        if node.id == source_id
            || node.id == gap_id
            || !in_context.contains(node.id.as_str())
            || spec::is_gap_node(node)
            // A configuration read (ADR-0059) is never the endpoint of the
            // edge a gap needs, so it is no candidate and takes no slot.
            || node.label == adapters_fw::config_read::CONFIG_READ
        {
            continue;
        }
        let provenance = spec::provenance(&node.props, &node.id);
        if provenance.confidence_tier == ConfidenceTier::Gap || provenance.evidence.is_empty() {
            continue;
        }
        if requests.len() == MAX_REQUESTS {
            unplanned_candidates += 1;
        } else {
            requests.push(request(node, true)?);
        }
    }
    Ok(TaskPlan {
        action_id: action_id.to_owned(),
        gap_id: gap_id.to_owned(),
        source_id: source_id.to_owned(),
        edge_label: edge_label.to_owned(),
        requests,
        slot: slot
            .map(|edge| Ok((FactKey::from_edge(edge), edge_digest(edge)?)))
            .transpose()
            .map_err(|_: core_graph::GraphError| "Task slot is invalid.")?,
        unplanned_candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prov(confidence: &str, path: &str) -> serde_json::Value {
        json!({
            "tier": "Deterministic",
            "confidence_tier": confidence,
            "evidence": [{
                "repo": "local/fixture",
                "path": path,
                "byte_start": 0,
                "byte_end": 24,
                "commit_sha": "workdir",
            }],
            "extractor_id": "t0.adapter-ts",
            "content_hash": "a".repeat(64),
        })
    }

    fn node(id: &str, label: &str, confidence: &str) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            props: json!({"name": id, "prov": prov(confidence, "src/a.ts")}),
        }
    }

    fn edge(src: &str, dst: &str, label: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: json!({"prov": prov("Confirmed", "src/a.ts")}),
        }
    }

    fn candidate_ids(plan: &TaskPlan) -> Vec<&str> {
        plan.requests
            .iter()
            .filter_map(|request| request.candidate.as_ref())
            .map(|candidate| candidate.node_id.as_str())
            .collect()
    }

    // AC-0597 (#1789, ADR-0059): a gap whose source symbol also reads
    // configuration gets its call neighbour as a candidate and never the
    // `ConfigRead` node, which sorts first by id and would otherwise take a
    // slot from a real target.
    #[test]
    fn config_reads_are_never_gap_candidates() {
        let nodes = vec![
            node(
                "cfgread:local/fixture@src/a.ts:env:API_URL",
                "ConfigRead",
                "Confirmed",
            ),
            node("gap:call", "Gap", "Gap"),
            node("sym:caller", "Symbol", "Confirmed"),
            node("sym:helper", "Symbol", "Confirmed"),
        ];
        let edges = vec![
            edge("sym:caller", "gap:call", "CALLS"),
            edge("sym:caller", "sym:helper", "CALLS"),
            edge(
                "sym:caller",
                "cfgread:local/fixture@src/a.ts:env:API_URL",
                "READS_CONFIG",
            ),
        ];
        let plan = plan(&nodes, &edges, "gap:call", "escalate:test", 2).unwrap();
        assert_eq!(plan.source_id, "sym:caller");
        assert_eq!(candidate_ids(&plan), ["sym:helper"]);
        assert_eq!(plan.unplanned_candidates, 0);
    }

    // AC-0597: past the request limit only real candidates are counted as
    // unplanned; configuration reads are not candidates at all.
    #[test]
    fn config_reads_are_not_counted_as_unplanned_candidates() {
        let mut nodes = vec![
            node("gap:call", "Gap", "Gap"),
            node("sym:caller", "Symbol", "Confirmed"),
        ];
        let mut edges = vec![edge("sym:caller", "gap:call", "CALLS")];
        for index in 0..MAX_REQUESTS {
            let id = format!("cfgread:local/fixture@src/a.ts:env:KEY_{index:03}");
            nodes.push(node(&id, "ConfigRead", "Confirmed"));
            edges.push(edge("sym:caller", &id, "READS_CONFIG"));
        }
        // Two requests are the source and the gap; three more overflow.
        for index in 0..MAX_REQUESTS + 1 {
            let id = format!("sym:helper_{index:03}");
            nodes.push(node(&id, "Symbol", "Confirmed"));
            edges.push(edge("sym:caller", &id, "CALLS"));
        }
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        let plan = plan(&nodes, &edges, "gap:call", "escalate:test", 2).unwrap();
        let candidates = candidate_ids(&plan);
        assert_eq!(candidates.len(), MAX_REQUESTS - 2);
        assert!(candidates.iter().all(|id| id.starts_with("sym:helper_")));
        assert_eq!(plan.unplanned_candidates, 3);
    }

    // AC-1418 (#1653): the saved T3 search radius bounds which facts are
    // candidates: a fact three hops from the gap is a candidate at radius 3
    // and not at the default radius 2.
    #[test]
    fn context_hops_bound_the_candidate_neighbourhood() {
        let nodes = vec![
            node("gap:call", "Gap", "Gap"),
            node("sym:a", "Symbol", "Confirmed"),
            node("sym:b", "Symbol", "Confirmed"),
            node("sym:c", "Symbol", "Confirmed"),
        ];
        let edges = vec![
            edge("sym:a", "gap:call", "CALLS"),
            edge("sym:a", "sym:b", "CALLS"),
            edge("sym:b", "sym:c", "CALLS"),
        ];
        let near = plan(&nodes, &edges, "gap:call", "escalate:test", 2).unwrap();
        assert_eq!(candidate_ids(&near), ["sym:b"]);
        let far = plan(&nodes, &edges, "gap:call", "escalate:test", 3).unwrap();
        assert_eq!(candidate_ids(&far), ["sym:b", "sym:c"]);
    }
}
