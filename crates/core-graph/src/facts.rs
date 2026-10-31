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
        self.nodes_where(|node| labels.contains(&node.label.as_str()))
    }

    /// Decode the nodes `select` picks by packed identity, in store order.
    pub fn nodes_where(
        &self,
        select: impl Fn(&PackedNode) -> bool,
    ) -> Result<Vec<Node>, GraphError> {
        self.nodes
            .iter()
            .filter(|node| select(node))
            .map(PackedNode::unpack)
            .collect()
    }

    /// Decode the edges `select` picks by packed identity, in store order.
    pub fn edges_where(
        &self,
        select: impl Fn(&PackedEdge) -> bool,
    ) -> Result<Vec<Edge>, GraphError> {
        self.edges
            .iter()
            .filter(|edge| select(edge))
            .map(PackedEdge::unpack)
            .collect()
    }

    /// Run `stage` over the nodes whose label is in `labels`, in store order.
    ///
    /// The stage edits the selected nodes in place through a slice, so it
    /// cannot remove or add any of them, and pushes new nodes onto `added`.
    /// Edited nodes go back into their slots and added nodes are appended
    /// after every held node. This equals running the stage over the whole
    /// node list when the stage reads only the selected labels, edits those
    /// nodes in place and appends; a stage that removes nodes cannot use it.
    pub fn update_nodes<T>(
        &mut self,
        labels: &[&str],
        stage: impl FnOnce(&mut [Node], &mut Vec<Node>) -> T,
    ) -> Result<T, GraphError> {
        self.update_nodes_where(|node| labels.contains(&node.label.as_str()), stage)
    }

    /// [`FactStore::update_nodes`] over the nodes `select` picks by packed
    /// identity (id and label), chosen before the stage runs.
    pub fn update_nodes_where<T>(
        &mut self,
        select: impl Fn(&PackedNode) -> bool,
        stage: impl FnOnce(&mut [Node], &mut Vec<Node>) -> T,
    ) -> Result<T, GraphError> {
        update_where(
            &mut self.nodes,
            select,
            PackedNode::unpack,
            PackedNode::pack,
            stage,
        )
    }

    /// [`FactStore::update_nodes_where`] for edges: the stage may edit any
    /// field of a selected edge, including its endpoints, and the edge keeps
    /// its slot.
    pub fn update_edges_where<T>(
        &mut self,
        select: impl Fn(&PackedEdge) -> bool,
        stage: impl FnOnce(&mut [Edge], &mut Vec<Edge>) -> T,
    ) -> Result<T, GraphError> {
        update_where(
            &mut self.edges,
            select,
            PackedEdge::unpack,
            PackedEdge::pack,
            stage,
        )
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

/// Select, decode, run and repack: everything is packed before the store is
/// written, so a refused fact leaves it as it was.
fn update_where<P, F, T>(
    facts: &mut Vec<P>,
    select: impl Fn(&P) -> bool,
    unpack: impl Fn(&P) -> Result<F, GraphError>,
    pack: impl Fn(F) -> Result<P, GraphError>,
    stage: impl FnOnce(&mut [F], &mut Vec<F>) -> T,
) -> Result<T, GraphError> {
    let slots: Vec<usize> = (0..facts.len())
        .filter(|&index| select(&facts[index]))
        .collect();
    let mut selected = slots
        .iter()
        .map(|&index| unpack(&facts[index]))
        .collect::<Result<Vec<_>, _>>()?;
    let mut added = Vec::new();
    let output = stage(&mut selected, &mut added);
    let selected = selected
        .into_iter()
        .map(&pack)
        .collect::<Result<Vec<_>, _>>()?;
    let added = added
        .into_iter()
        .map(&pack)
        .collect::<Result<Vec<_>, _>>()?;
    for (&index, fact) in slots.iter().zip(selected) {
        facts[index] = fact;
    }
    facts.extend(added);
    Ok(output)
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
    fn enrich(nodes: &mut [Node], added: &mut Vec<Node>) -> usize {
        let endpoints = nodes.iter().filter(|n| n.label == "Endpoint").count();
        for node in nodes.iter_mut().filter(|n| n.label == "Endpoint") {
            node.props["enriched"] = json!(true);
        }
        added.push(node("ep:new", "Endpoint", 9));
        endpoints
    }

    #[test]
    fn fact_store_stages_match_whole_list_stages_and_keep_order() {
        // AC-0527: a label-selected stage equals the same stage over the whole
        // list, slots and order are kept, and a fact that cannot be packed
        // leaves the store unchanged.
        let nodes = vec![
            node("sym:a", "Symbol", 1),
            node("ep:a", "Endpoint", 2),
            node("dup", "Symbol", 3),
            node("ep:b", "Endpoint", 4),
            node("dup", "Endpoint", 5),
        ];
        let edges = vec![edge("sym:a", "ep:a"), edge("dup", "ep:b")];

        let mut whole = nodes.clone();
        let mut appended = Vec::new();
        let expected_count = enrich(&mut whole, &mut appended);
        whole.extend(appended);

        let mut store = FactStore::from_facts(nodes, edges.clone()).unwrap();
        let count = store.update_nodes(&["Endpoint"], enrich).unwrap();
        assert_eq!(count, expected_count);
        assert_eq!(
            store.nodes_labeled(&["Endpoint"]).unwrap().len(),
            whole.iter().filter(|n| n.label == "Endpoint").count()
        );
        assert_eq!(store.clone().into_facts().unwrap(), (whole, edges));

        // Reordering selected nodes permutes their slots, as the same
        // permutation of their positions in the whole list would.
        let mut reordered = FactStore::from_facts(
            vec![
                node("ep:a", "Endpoint", 1),
                node("sym:s", "Symbol", 2),
                node("ep:b", "Endpoint", 3),
            ],
            Vec::new(),
        )
        .unwrap();
        reordered
            .update_nodes(&["Endpoint"], |nodes, added| {
                nodes.reverse();
                added.push(node("ep:n", "Endpoint", 4));
            })
            .unwrap();
        let ids: Vec<_> = reordered.nodes().iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["ep:b", "sym:s", "ep:a", "ep:n"]);

        let before = store.clone().into_facts().unwrap();
        let mut deep = json!(0);
        for _ in 0..=crate::packed::MAX_PROPS_DEPTH {
            deep = json!([deep]);
        }
        let refused = store.update_nodes(&["Endpoint"], |nodes, added| {
            nodes[0].props["edited"] = json!(true);
            added.push(Node {
                id: "ep:deep".into(),
                label: "Endpoint".into(),
                props: deep,
            });
        });
        assert!(matches!(refused, Err(GraphError::FactStore(_))));
        // A refused stage leaves the store as it was.
        assert_eq!(store.into_facts().unwrap(), before);
    }

    #[test]
    fn predicate_stages_match_whole_list_stages_and_keep_slots() {
        // AC-0530: identity-selected node and edge stages equal the same
        // stages over the whole lists; edited edges keep their slots even
        // when their endpoints change, and a refused edge stage leaves the
        // store unchanged.
        let registered = ["file:r@sw.ts", "sym:r@sw.ts#on"];
        let nodes = vec![
            node("file:r@a.ts", "File", 1),
            node("file:r@sw.ts", "File", 2),
            node("sym:r@a.ts#f", "Symbol", 3),
            node("sym:r@sw.ts#on", "Symbol", 4),
        ];
        let import = |src: &str, dst: &str| Edge {
            label: "IMPORTS".into(),
            ..edge(src, dst)
        };
        let edges = vec![
            import("file:r@a.ts", "mod:lib"),
            edge("sym:r@a.ts#f", "sym:r@sw.ts#on"),
            import("file:r@sw.ts", "file:r@a.ts"),
            import("file:r@sw.ts", "mod:other"),
        ];
        let mark = |node: &mut Node| node.props["runtime"] = json!("service-worker");
        let retarget = |edge: &mut Edge| {
            edge.dst = edge.dst.replace("mod:", "file:r@node_modules/");
            edge.props["resolved_via"] = json!("workspace");
        };
        let is_unresolved_import =
            |label: &str, dst: &str| label == "IMPORTS" && dst.starts_with("mod:");

        let (mut whole_nodes, mut whole_edges) = (nodes.clone(), edges.clone());
        whole_nodes
            .iter_mut()
            .filter(|n| registered.contains(&n.id.as_str()))
            .for_each(mark);
        whole_edges
            .iter_mut()
            .filter(|e| is_unresolved_import(&e.label, &e.dst))
            .for_each(retarget);
        whole_edges.push(edge("sym:r@sw.ts#on", "sym:r@a.ts#f"));

        let mut store = FactStore::from_facts(nodes, edges).unwrap();
        let marked = store
            .update_nodes_where(
                |n| registered.contains(&n.id.as_str()),
                |selected, _| {
                    selected.iter_mut().for_each(mark);
                    selected.len()
                },
            )
            .unwrap();
        assert_eq!(marked, 2);
        store
            .update_edges_where(
                |e| is_unresolved_import(&e.label, &e.dst),
                |selected, added| {
                    selected.iter_mut().for_each(retarget);
                    added.push(edge("sym:r@sw.ts#on", "sym:r@a.ts#f"));
                },
            )
            .unwrap();
        assert_eq!(
            store
                .nodes_where(|n| registered.contains(&n.id.as_str()))
                .unwrap(),
            whole_nodes
                .iter()
                .filter(|n| registered.contains(&n.id.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        );
        assert_eq!(
            store.edges_where(|e| e.label == "IMPORTS").unwrap().len(),
            3
        );
        assert_eq!(
            store.clone().into_facts().unwrap(),
            (whole_nodes, whole_edges)
        );

        let before = store.clone().into_facts().unwrap();
        let mut deep = json!(0);
        for _ in 0..=crate::packed::MAX_PROPS_DEPTH {
            deep = json!([deep]);
        }
        let refused = store.update_edges_where(
            |e| e.label == "IMPORTS",
            |selected, _| selected[0].props = deep,
        );
        assert!(matches!(refused, Err(GraphError::FactStore(_))));
        assert_eq!(store.into_facts().unwrap(), before);
    }
}
