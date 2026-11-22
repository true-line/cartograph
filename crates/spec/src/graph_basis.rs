//! Graph content identity (#1765, AC-0593): the one definition of the graph
//! content hash, shared by the per-ingest history record (AC-0060) and the
//! export manifest, so a hash recorded at ingest and a hash carried by an
//! export are comparable.
//!
//! The hash covers base graph facts only: each node's id, label and
//! provenance content hash, and each edge's `(src, label, dst)` and provenance
//! content hash. Curation decisions and knowledge-overlay facts live outside
//! the base graph and never enter it.
//!
//! Scope (AGENTS.md "Determinism"): equality is claimed per source
//! registration. Fact ids embed the registry key, and direct-local and
//! `file://` sources carry random `local/` keys (ADR-0023), so two
//! installations never share a hash for those; only GitHub-backed managed
//! sources have stable `owner/name` keys.

use crate::workbench::{edge_identity, provenance};
use core_graph::{Edge, Node};
use core_prov::Provenance;
use serde::{Deserialize, Serialize};

/// One source registration and the revision its facts were recovered at.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceRevision {
    /// Registry key of the source (`owner/name`, or a random `local/` key
    /// that is meaningful on this installation only).
    pub repo: String,
    /// Commit the facts cite, or `workdir` for an uncommitted local tree.
    pub commit: String,
}

/// What an export was compiled from: the content hash of the whole base graph
/// and the source revisions that graph holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphBasis {
    /// [`graph_content_hash`] of every base fact in the exported graph.
    pub content_hash: String,
    /// Source registrations in the graph, ordered by registry key.
    pub sources: Vec<SourceRevision>,
}

/// Canonical hash line of one node: identity, label and content hash.
pub fn node_fact_line(node: &Node, provenance: &Provenance) -> String {
    format!("n {} {} {}", node.id, node.label, provenance.content_hash)
}

/// Canonical hash line of one edge: identity and content hash.
pub fn edge_fact_line(edge: &Edge, provenance: &Provenance) -> String {
    format!("e {} {}", edge_identity(edge), provenance.content_hash)
}

/// BLAKE3 over the sorted fact lines joined by a newline. Sorting makes the
/// result independent of store and iteration order; the lines are hashed as
/// they are visited, so no second copy of them is built.
pub fn hash_fact_lines(mut lines: Vec<String>) -> String {
    lines.sort_unstable();
    let mut hasher = blake3::Hasher::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            hasher.update(b"\n");
        }
        hasher.update(line.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// Canonical fact lines held end to end in one buffer, so a graph of
/// millions of facts holds its lines' bytes and one span per line instead of
/// one allocation per line (#2215). [`FactLines::hash`] is
/// [`hash_fact_lines`] of the same lines.
#[derive(Debug, Default)]
pub struct FactLines {
    text: String,
    spans: Vec<(usize, usize)>,
}

impl FactLines {
    /// Room for `lines` spans.
    pub fn with_capacity(lines: usize) -> Self {
        Self {
            text: String::new(),
            spans: Vec::with_capacity(lines),
        }
    }

    /// Append the line `write` writes to the buffer.
    pub fn push_with(&mut self, write: impl FnOnce(&mut String)) {
        let start = self.text.len();
        write(&mut self.text);
        self.spans.push((start, self.text.len()));
    }

    /// [`node_fact_line`] of `node`.
    pub fn push_node(&mut self, node: &Node, provenance: &Provenance) {
        self.push_with(|text| {
            for part in [
                "n ",
                &node.id,
                " ",
                &node.label,
                " ",
                &provenance.content_hash,
            ] {
                text.push_str(part);
            }
        });
    }

    /// [`edge_fact_line`] of `edge`.
    pub fn push_edge(&mut self, edge: &Edge, provenance: &Provenance) {
        self.push_with(|text| {
            for part in [
                "e ",
                &edge.src,
                " ",
                &edge.label,
                " ",
                &edge.dst,
                " ",
                &provenance.content_hash,
            ] {
                text.push_str(part);
            }
        });
    }

    /// The hash of the held lines, sorted and joined by a newline.
    pub fn hash(mut self) -> String {
        let text = self.text.as_bytes();
        self.spans
            .sort_unstable_by(|a, b| text[a.0..a.1].cmp(&text[b.0..b.1]));
        let mut hasher = blake3::Hasher::new();
        for (index, &(start, end)) in self.spans.iter().enumerate() {
            if index > 0 {
                hasher.update(b"\n");
            }
            hasher.update(&text[start..end]);
        }
        hasher.finalize().to_hex().to_string()
    }
}

/// Content hash of every given base fact, in any order.
pub fn graph_content_hash(nodes: &[Node], edges: &[Edge]) -> String {
    let mut lines = FactLines::with_capacity(nodes.len() + edges.len());
    for node in nodes {
        lines.push_node(node, &provenance(&node.props, &node.id));
    }
    for edge in edges {
        lines.push_edge(edge, &provenance(&edge.props, &edge_identity(edge)));
    }
    lines.hash()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, content: &str) -> Node {
        let prov = Provenance::new(
            core_prov::Tier::Deterministic,
            core_prov::ConfidenceTier::Confirmed,
            vec![],
            "t0.fixture",
            content.as_bytes(),
        )
        .unwrap();
        Node {
            id: id.into(),
            label: "Symbol".into(),
            props: json!({ "prov": prov }),
        }
    }

    fn edge(src: &str, dst: &str, content: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: "CALLS".into(),
            props: node("", content).props,
        }
    }

    // AC-0593: the hash is the recorded AC-0060 value (sorted lines joined by
    // a newline), independent of fact order and sensitive to content.
    #[test]
    fn graph_content_hash_is_order_independent_and_keeps_the_recorded_formula() {
        let nodes = [node("sym:b", "b"), node("sym:a", "a")];
        let edges = [edge("sym:b", "sym:a", "ba"), edge("sym:a", "sym:b", "ab")];
        let hash = graph_content_hash(&nodes, &edges);

        let mut lines: Vec<String> = nodes
            .iter()
            .map(|n| node_fact_line(n, &provenance(&n.props, &n.id)))
            .chain(
                edges
                    .iter()
                    .map(|e| edge_fact_line(e, &provenance(&e.props, &edge_identity(e)))),
            )
            .collect();
        lines.sort();
        assert_eq!(hash, core_prov::content_hash(lines.join("\n").as_bytes()));
        assert_eq!(
            graph_content_hash(&[], &[]),
            core_prov::content_hash(b""),
            "an empty graph hashes the empty input"
        );

        let reversed_nodes = [nodes[1].clone(), nodes[0].clone()];
        let reversed_edges = [edges[1].clone(), edges[0].clone()];
        assert_eq!(hash, graph_content_hash(&reversed_nodes, &reversed_edges));

        let changed = [node("sym:b", "b"), node("sym:a", "changed")];
        assert_ne!(hash, graph_content_hash(&changed, &edges));
        assert_ne!(hash, graph_content_hash(&nodes, &edges[..1]));
    }

    // AC-1210 (T-1210): lines held in one buffer hash as the sorted lines
    // joined by a newline, whatever their order, lengths and shared prefixes.
    #[test]
    fn fact_lines_in_one_buffer_hash_as_the_sorted_lines() {
        let lines = ["n b", "n a b", "n a", "", "e a CALLS b x", "n é", "n a"];
        let mut held = FactLines::default();
        for line in lines {
            held.push_with(|text| text.push_str(line));
        }
        let mut sorted = lines.map(String::from).to_vec();
        sorted.sort();
        assert_eq!(
            held.hash(),
            core_prov::content_hash(sorted.join("\n").as_bytes())
        );
        assert_eq!(
            hash_fact_lines(lines.map(String::from).to_vec()),
            core_prov::content_hash(sorted.join("\n").as_bytes())
        );
        let node = node("sym:a", "a");
        let mut one = FactLines::default();
        one.push_node(&node, &provenance(&node.props, &node.id));
        let edge = edge("sym:a", "sym:b", "ab");
        one.push_edge(&edge, &provenance(&edge.props, &edge_identity(&edge)));
        assert_eq!(
            one.hash(),
            hash_fact_lines(vec![
                node_fact_line(&node, &provenance(&node.props, &node.id)),
                edge_fact_line(&edge, &provenance(&edge.props, &edge_identity(&edge))),
            ])
        );
    }

    // AC-0593: a bundle compiled without a basis serializes exactly as before,
    // and a stamped basis round-trips.
    #[test]
    fn bundle_graph_basis_is_additive_and_round_trips() {
        let mut bundle = crate::compile_spec(
            &[node("sym:a", "a")],
            &[],
            &[],
            crate::ExportMode::VerifiedOnly,
            &Default::default(),
        );
        assert_eq!(bundle.graph_basis, None);
        let plain = serde_json::to_value(&bundle).unwrap();
        assert!(plain.get("graph_basis").is_none());
        assert_eq!(
            serde_json::from_value::<crate::SpecBundle>(plain).unwrap(),
            bundle
        );

        bundle.graph_basis = Some(GraphBasis {
            content_hash: graph_content_hash(&[node("sym:a", "a")], &[]),
            sources: vec![SourceRevision {
                repo: "acme/shop".into(),
                commit: "a".repeat(40),
            }],
        });
        let stamped = serde_json::to_value(&bundle).unwrap();
        assert_eq!(stamped["graph_basis"]["sources"][0]["repo"], "acme/shop");
        assert_eq!(
            serde_json::from_value::<crate::SpecBundle>(stamped).unwrap(),
            bundle
        );
    }
}
