//! The complete-graph `context-v1` identity, computed by streaming (#1602).
//!
//! [`ContextSnapshot::new`] hashes the whole sorted graph at once, so deriving
//! its id has needed the whole graph in memory. This builder produces the same
//! id from facts delivered one at a time in identity order, so a scoped
//! investigation can keep the exact complete-graph identity that ledgers and
//! drafting staging compare while retaining only its own scope.
use crate::{ContextError, ContextSnapshot, canonicalize, edge_reference};
use core_graph::{
    Edge, GraphError, IdentityPassLimits, Node, SnapshotRow, SqliteGraphStore,
    scoped::ScopedGraphReader,
};

const DOMAIN: &str = "context-v1";

/// Incremental form of the id [`ContextSnapshot::new`] assigns.
///
/// Nodes come first in strictly increasing `id` order, then edges in strictly
/// increasing `(src, label, dst)` order, matching the order the snapshot sorts
/// into. Out-of-order input is refused rather than hashed differently, and an
/// equal key is the same duplicate refusal the snapshot gives.
pub struct ContextIdentityBuilder {
    hasher: blake3::Hasher,
    last_node: Option<String>,
    last_edge: Option<(String, String, String)>,
}

impl Default for ContextIdentityBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextIdentityBuilder {
    /// Start an empty complete graph.
    pub fn new() -> Self {
        let mut hasher = blake3::Hasher::new();
        // `serde_json::to_vec(&(DOMAIN, [[nodes...], [edges...]]))`, compact.
        hasher.update(b"[");
        serde_json::to_writer(&mut hasher, DOMAIN).expect("hashing a str cannot fail");
        hasher.update(b",[[");
        Self {
            hasher,
            last_node: None,
            last_edge: None,
        }
    }

    /// Add the next node. Every node must precede every edge.
    pub fn push_node(&mut self, node: &Node) -> Result<(), ContextError> {
        if self.last_edge.is_some() {
            return Err(ContextError::UnorderedInput);
        }
        match &self.last_node {
            Some(last) if *last == node.id => {
                return Err(ContextError::DuplicateNode(node.id.clone()));
            }
            Some(last) if *last > node.id => return Err(ContextError::UnorderedInput),
            Some(_) => {
                self.hasher.update(b",");
            }
            None => {}
        }
        self.write(node)?;
        self.last_node = Some(node.id.clone());
        Ok(())
    }

    /// Add the next edge.
    pub fn push_edge(&mut self, edge: &Edge) -> Result<(), ContextError> {
        let key = (edge.src.clone(), edge.label.clone(), edge.dst.clone());
        match &self.last_edge {
            Some(last) if *last == key => {
                return Err(ContextError::DuplicateEdge(edge_reference(edge)));
            }
            Some(last) if *last > key => return Err(ContextError::UnorderedInput),
            Some(_) => {
                self.hasher.update(b",");
            }
            None => {
                self.hasher.update(b"],[");
            }
        }
        self.write(edge)?;
        self.last_edge = Some(key);
        Ok(())
    }

    /// The id the complete graph pushed so far would have as a snapshot.
    pub fn finish(mut self) -> String {
        if self.last_edge.is_none() {
            self.hasher.update(b"],[");
        }
        self.hasher.update(b"]]]");
        format!("{DOMAIN}:{}", self.hasher.finalize().to_hex())
    }

    fn write(&mut self, fact: &impl serde::Serialize) -> Result<(), ContextError> {
        // Exactly the per-fact canonical form `content_identity` serializes.
        let canonical = canonicalize(serde_json::to_value(fact)?);
        serde_json::to_writer(&mut self.hasher, &canonical)?;
        Ok(())
    }

    fn push(&mut self, row: &SnapshotRow) -> Result<(), ContextError> {
        match row {
            SnapshotRow::Node(node) => self.push_node(node),
            SnapshotRow::Edge(edge) => self.push_edge(edge),
        }
    }
}

/// A complete-graph identity could not be streamed.
#[derive(Debug, thiserror::Error)]
pub enum CompleteIdentityError {
    /// The store read failed or the graph exceeded the selected limits.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// The streamed facts were not a valid complete snapshot input.
    #[error(transparent)]
    Context(#[from] ContextError),
}

impl ContextSnapshot {
    /// The id [`ContextSnapshot::new`] would assign to the complete stored
    /// graph, read in one revision, retaining one row at a time. A plain
    /// [`core_graph::SnapshotReadLimits`] gives the same admission as
    /// [`SqliteGraphStore::read_snapshot_bounded`];
    /// [`IdentityPassLimits::for_retained`] gives the preparation caps
    /// (ADR-0052 amendment 2026-11-18).
    pub fn complete_id_bounded(
        store: &SqliteGraphStore,
        limits: impl Into<IdentityPassLimits>,
    ) -> Result<String, CompleteIdentityError> {
        let mut builder = ContextIdentityBuilder::new();
        store.visit_snapshot_bounded(limits, |row| {
            builder.push(&row).map_err(CompleteIdentityError::from)
        })?;
        Ok(builder.finish())
    }

    /// [`Self::complete_id_bounded`] inside a scoped reader's pinned
    /// transaction, so the id and the scoped facts describe one revision.
    pub fn complete_id_in(
        reader: &ScopedGraphReader<'_>,
        limits: impl Into<IdentityPassLimits>,
    ) -> Result<String, CompleteIdentityError> {
        let mut builder = ContextIdentityBuilder::new();
        reader.visit_complete_snapshot(limits, |row| {
            builder.push(&row).map_err(CompleteIdentityError::from)
        })?;
        Ok(builder.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graph::{GraphStore, SnapshotReadLimits};
    use serde_json::json;

    fn node(id: &str, props: serde_json::Value) -> Node {
        Node {
            id: id.into(),
            label: "Symbol".into(),
            props,
        }
    }

    fn edge(src: &str, label: &str, dst: &str) -> Edge {
        Edge {
            src: src.into(),
            label: label.into(),
            dst: dst.into(),
            props: json!({"z": [2, 1], "a": {"y": true, "b": "\u{2028}\"\\"}}),
        }
    }

    fn graph() -> (Vec<Node>, Vec<Edge>) {
        (
            vec![
                node(
                    "svc/b",
                    json!({"z": 1, "a": [1.5, -0.0, 1e300, {"k": null}]}),
                ),
                node(
                    "svc/a",
                    json!({"name": "caf\u{e9}", "prov": {"tier": "T1"}}),
                ),
                node("svc/c", serde_json::Value::Null),
            ],
            vec![
                edge("svc/a", "Z", "svc/b"),
                edge("svc/a", "A", "svc/c"),
                edge("svc/b", "CALLS", "svc/a"),
            ],
        )
    }

    fn store(graph: &(Vec<Node>, Vec<Edge>)) -> SqliteGraphStore {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for node in &graph.0 {
            store.put_node(node).unwrap();
        }
        for edge in &graph.1 {
            store.put_edge(edge).unwrap();
        }
        store
    }

    fn snapshot_id(store: &SqliteGraphStore) -> String {
        let (nodes, edges) = store
            .read_snapshot_bounded(SnapshotReadLimits::default())
            .unwrap();
        ContextSnapshot::new(nodes, edges).unwrap().id().to_owned()
    }

    // T-0735 (#1602): the streamed complete-graph id is byte-for-byte the id
    // ContextSnapshot::new assigns, so ledgers and drafting staging compare
    // the same graph_snapshot_id; it is also stable inside a scoped reader.
    #[test]
    fn streamed_identity_equals_the_materialized_snapshot_id() {
        let graph = graph();
        let store = store(&graph);
        let expected = snapshot_id(&store);
        assert_eq!(
            ContextSnapshot::new(graph.0.clone(), graph.1.clone())
                .unwrap()
                .id(),
            expected
        );
        assert_eq!(
            ContextSnapshot::complete_id_bounded(&store, SnapshotReadLimits::default()).unwrap(),
            expected
        );
        let revision = store.read_revision().unwrap();
        let (inside, neighborhood) = store
            .with_scoped_reader(&revision, SnapshotReadLimits::default(), |reader| {
                let id = ContextSnapshot::complete_id_in(reader, SnapshotReadLimits::default())?;
                let hood = reader
                    .neighborhood("svc/a", 1)
                    .map_err(CompleteIdentityError::from)?;
                Ok::<_, CompleteIdentityError>((id, hood))
            })
            .unwrap();
        assert_eq!(inside, expected);
        assert!(neighborhood.is_some());
        for (nodes, edges) in [
            (vec![], vec![]),
            (graph.0.clone(), vec![]),
            (vec![graph.0[0].clone()], vec![]),
        ] {
            let empty_store = store_of(&nodes, &edges);
            assert_eq!(
                ContextSnapshot::complete_id_bounded(&empty_store, SnapshotReadLimits::default())
                    .unwrap(),
                ContextSnapshot::new(nodes, edges).unwrap().id()
            );
        }
        // A different graph has a different id.
        let mut changed = graph.clone();
        changed.0[2].props = json!(1);
        assert_ne!(
            ContextSnapshot::complete_id_bounded(
                &store_of(&changed.0, &changed.1),
                SnapshotReadLimits::default()
            )
            .unwrap(),
            expected
        );
        // The same admission as the materialized read.
        let tight = SnapshotReadLimits {
            max_rows: 5,
            ..SnapshotReadLimits::default()
        };
        assert!(matches!(
            ContextSnapshot::complete_id_bounded(&store, tight),
            Err(CompleteIdentityError::Graph(
                core_graph::GraphError::SnapshotBounds(core_graph::SnapshotBoundsError::RowLimit)
            ))
        ));
    }

    // T-0775 (#1602, ADR-0052 amendment 2026-11-18): under the preparation
    // caps the complete id is computed for a graph past the retained limits,
    // inside a scoped reader held to those limits, and it is still exactly
    // the materialized snapshot id.
    #[test]
    fn preparation_caps_keep_the_complete_id_past_the_retained_limits() {
        let graph = graph();
        let store = store(&graph);
        let expected = snapshot_id(&store);
        let retained = SnapshotReadLimits {
            max_rows: 2,
            ..SnapshotReadLimits::default()
        };
        assert!(store.read_snapshot_bounded(retained).is_err());
        let caps = IdentityPassLimits::for_retained(retained);
        assert_eq!(
            ContextSnapshot::complete_id_bounded(&store, caps).unwrap(),
            expected
        );
        let revision = store.read_revision().unwrap();
        let inside = store
            .with_scoped_reader(&revision, retained, |reader| {
                ContextSnapshot::complete_id_in(
                    reader,
                    IdentityPassLimits::for_retained(reader.limits()),
                )
            })
            .unwrap();
        assert_eq!(inside, expected);
        // A single row past the retained bytes is refused, never hashed.
        assert!(matches!(
            ContextSnapshot::complete_id_bounded(
                &store,
                IdentityPassLimits::for_retained(SnapshotReadLimits {
                    max_bytes: 8,
                    ..retained
                })
            ),
            Err(CompleteIdentityError::Graph(
                core_graph::GraphError::SnapshotBounds(
                    core_graph::SnapshotBoundsError::SingleRowByteLimit
                )
            ))
        ));
    }

    fn store_of(nodes: &[Node], edges: &[Edge]) -> SqliteGraphStore {
        store(&(nodes.to_vec(), edges.to_vec()))
    }

    // T-0735 (#1602): the builder refuses input it would hash differently
    // from the snapshot: out of order, duplicate, or a node after an edge.
    #[test]
    fn builder_refuses_unordered_and_duplicate_input() {
        let (nodes, _) = graph();
        let mut builder = ContextIdentityBuilder::new();
        builder.push_node(&nodes[1]).unwrap(); // svc/a
        assert!(matches!(
            builder.push_node(&nodes[1]),
            Err(ContextError::DuplicateNode(_))
        ));
        builder.push_node(&nodes[0]).unwrap(); // svc/b
        assert!(matches!(
            builder.push_node(&nodes[1]),
            Err(ContextError::UnorderedInput)
        ));
        builder.push_edge(&edge("svc/a", "Z", "svc/b")).unwrap();
        assert!(matches!(
            builder.push_edge(&edge("svc/a", "A", "svc/c")),
            Err(ContextError::UnorderedInput)
        ));
        assert!(matches!(
            builder.push_edge(&edge("svc/a", "Z", "svc/b")),
            Err(ContextError::DuplicateEdge(_))
        ));
        assert!(matches!(
            builder.push_node(&nodes[2]),
            Err(ContextError::UnorderedInput)
        ));
    }
}
