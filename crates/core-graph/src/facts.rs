//! An order-preserving store of packed facts (#725 item 3).
//!
//! Recovery stages each read a few labels of a graph whose bulk they never
//! inspect. The store keeps every fact packed and decodes only what a stage
//! selects: `update_nodes` hands the selected nodes, in store order, to an
//! unchanged stage and puts its result back in their slots. Facts keep their
//! relative order, which publication relies on (last occurrence wins).
use crate::packed::{PackedEdge, PackedNode};
use crate::{Edge, GraphError, Node};

/// Packed nodes and edges in deterministic recovery order.
#[derive(Debug, Clone, Default)]
pub struct FactStore {
    nodes: Vec<PackedNode>,
    edges: Vec<PackedEdge>,
}

impl FactStore {
    /// Pack `nodes` and `edges`, keeping their order.
    pub fn from_facts(nodes: Vec<Node>, edges: Vec<Edge>) -> Result<Self, GraphError> {
        let mut store = Self::default();
        store.extend(nodes, edges)?;
        Ok(store)
    }

    /// Append facts after every fact already held, packing each as it moves.
    pub fn extend(
        &mut self,
        nodes: impl IntoIterator<Item = Node>,
        edges: impl IntoIterator<Item = Edge>,
    ) -> Result<(), GraphError> {
        for node in nodes {
            self.nodes.push(PackedNode::pack(node)?);
        }
        for edge in edges {
            self.edges.push(PackedEdge::pack(edge)?);
        }
        Ok(())
    }

    /// Packed nodes in order; identity is readable without decoding.
    pub fn nodes(&self) -> &[PackedNode] {
        &self.nodes
    }

    /// Packed edges in order; endpoints are readable without decoding.
    pub fn edges(&self) -> &[PackedEdge] {
        &self.edges
    }

    /// Decode the nodes whose label is in `labels`, in store order.
    pub fn nodes_labeled(&self, labels: &[&str]) -> Result<Vec<Node>, GraphError> {
        self.nodes
            .iter()
            .filter(|node| labels.contains(&node.label.as_str()))
            .map(PackedNode::unpack)
            .collect()
    }

    /// Run `stage` over the nodes whose label is in `labels`, in store order.
    ///
    /// The first `k` results replace the `k` selected slots in place, and any
    /// further results are appended after every held node. This equals running
    /// the stage over the whole node list when the stage only reads the
    /// selected labels, edits them in place and appends; a stage that removes
    /// selected nodes must not use it.
    pub fn update_nodes<T>(
        &mut self,
        labels: &[&str],
        stage: impl FnOnce(&mut Vec<Node>) -> T,
    ) -> Result<T, GraphError> {
        let slots: Vec<usize> = (0..self.nodes.len())
            .filter(|&index| labels.contains(&self.nodes[index].label.as_str()))
            .collect();
        let mut selected = slots
            .iter()
            .map(|&index| self.nodes[index].unpack())
            .collect::<Result<Vec<_>, _>>()?;
        let output = stage(&mut selected);
        if selected.len() < slots.len() {
            return Err(GraphError::FactStore("a stage removed selected nodes"));
        }
        let mut results = selected.into_iter();
        for (&index, node) in slots.iter().zip(results.by_ref()) {
            self.nodes[index] = PackedNode::pack(node)?;
        }
        for node in results {
            self.nodes.push(PackedNode::pack(node)?);
        }
        Ok(output)
    }

    /// Decode every fact, in order.
    pub fn into_facts(self) -> Result<(Vec<Node>, Vec<Edge>), GraphError> {
        Ok((
            self.nodes
                .iter()
                .map(PackedNode::unpack)
                .collect::<Result<_, _>>()?,
            self.edges
                .iter()
                .map(PackedEdge::unpack)
                .collect::<Result<_, _>>()?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, label: &str, n: u64) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            props: json!({"n": n, "nested": {"b": [n], "a": null}}),
        }
    }

    fn edge(src: &str, dst: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: "CALLS".into(),
            props: json!({"w": 1.5}),
        }
    }

    /// A stage that reads only Endpoints, edits them in place and appends.
    fn enrich(nodes: &mut Vec<Node>) -> usize {
        let endpoints = nodes.iter().filter(|n| n.label == "Endpoint").count();
        for node in nodes.iter_mut().filter(|n| n.label == "Endpoint") {
            node.props["enriched"] = json!(true);
        }
        nodes.push(node("ep:new", "Endpoint", 9));
        endpoints
    }

    #[test]
    fn fact_store_stages_match_whole_list_stages_and_keep_order() {
        // AC-0527: a label-selected stage equals the same stage over the whole
        // list, slots and order are kept, and removal is refused.
        let nodes = vec![
            node("sym:a", "Symbol", 1),
            node("ep:a", "Endpoint", 2),
            node("dup", "Symbol", 3),
            node("ep:b", "Endpoint", 4),
            node("dup", "Endpoint", 5),
        ];
        let edges = vec![edge("sym:a", "ep:a"), edge("dup", "ep:b")];

        let mut whole = nodes.clone();
        let expected_count = enrich(&mut whole);

        let mut store = FactStore::from_facts(nodes, edges.clone()).unwrap();
        let count = store.update_nodes(&["Endpoint"], enrich).unwrap();
        assert_eq!(count, expected_count);
        assert_eq!(
            store.nodes_labeled(&["Endpoint"]).unwrap().len(),
            whole.iter().filter(|n| n.label == "Endpoint").count()
        );
        assert_eq!(store.clone().into_facts().unwrap(), (whole, edges));

        let before = store.clone().into_facts().unwrap();
        let refused = store.update_nodes(&["Endpoint"], |nodes| nodes.truncate(1));
        assert!(matches!(refused, Err(GraphError::FactStore(_))));
        // A refused stage leaves the store as it was.
        assert_eq!(store.into_facts().unwrap(), before);
    }
}
