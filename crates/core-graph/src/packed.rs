//! Lossless packed facts (#725 item 3, #1518).
//!
//! A `serde_json::Value` tree costs several times its serialized size, and a
//! large extraction holds hundreds of thousands of them until publication. A
//! packed fact keeps its identity strings and stores its props as the compact
//! JSON that publication already writes (`Map` is key-sorted: the workspace
//! does not enable `preserve_order`). Unpacking parses those bytes back to the
//! identical `Node` or `Edge`, so packing changes no fact, digest or row.
use crate::{Edge, GraphError, Node};
use serde_json::value::RawValue;

/// Deepest container nesting `serde_json::from_str` parses: its default
/// recursion limit admits 127 nested arrays or objects. Deeper props are
/// refused when packed, so every packed fact unpacks.
pub const MAX_PROPS_DEPTH: usize = 127;

fn packable(props: &serde_json::Value) -> Result<(), GraphError> {
    fn depth(value: &serde_json::Value, at: usize) -> bool {
        let children: Box<dyn Iterator<Item = &serde_json::Value>> = match value {
            serde_json::Value::Array(items) => Box::new(items.iter()),
            serde_json::Value::Object(fields) => Box::new(fields.values()),
            _ => return true,
        };
        at < MAX_PROPS_DEPTH && { children }.all(|child| depth(child, at + 1))
    }
    if depth(props, 0) {
        Ok(())
    } else {
        Err(GraphError::FactStore(
            "props nest deeper than a packed fact can unpack",
        ))
    }
}

/// A [`Node`] whose props are held as compact serialized JSON.
#[derive(Debug, Clone)]
pub struct PackedNode {
    /// Stable identifier, as on [`Node::id`].
    pub id: String,
    /// Node label, as on [`Node::label`].
    pub label: String,
    props: Box<RawValue>,
}

/// An [`Edge`] whose props are held as compact serialized JSON.
#[derive(Debug, Clone)]
pub struct PackedEdge {
    /// Source node id, as on [`Edge::src`].
    pub src: String,
    /// Destination node id, as on [`Edge::dst`].
    pub dst: String,
    /// Edge label, as on [`Edge::label`].
    pub label: String,
    props: Box<RawValue>,
}

impl PackedNode {
    /// Pack `node`, dropping its props tree once serialized.
    pub fn pack(node: Node) -> Result<Self, GraphError> {
        packable(&node.props)?;
        Ok(Self {
            props: serde_json::value::to_raw_value(&node.props)?,
            id: node.id,
            label: node.label,
        })
    }

    /// The identical [`Node`] this fact was packed from.
    pub fn unpack(&self) -> Result<Node, GraphError> {
        Ok(Node {
            id: self.id.clone(),
            label: self.label.clone(),
            props: serde_json::from_str(self.props.get())?,
        })
    }

    /// Props exactly as `serde_json::to_string(&node.props)` writes them.
    pub fn props_json(&self) -> &str {
        self.props.get()
    }
}

impl PackedEdge {
    /// Pack `edge`, dropping its props tree once serialized.
    pub fn pack(edge: Edge) -> Result<Self, GraphError> {
        packable(&edge.props)?;
        Ok(Self {
            props: serde_json::value::to_raw_value(&edge.props)?,
            src: edge.src,
            dst: edge.dst,
            label: edge.label,
        })
    }

    /// The identical [`Edge`] this fact was packed from.
    pub fn unpack(&self) -> Result<Edge, GraphError> {
        Ok(Edge {
            src: self.src.clone(),
            dst: self.dst.clone(),
            label: self.label.clone(),
            props: serde_json::from_str(self.props.get())?,
        })
    }

    /// Props exactly as `serde_json::to_string(&edge.props)` writes them.
    pub fn props_json(&self) -> &str {
        self.props.get()
    }
}

/// A full graph snapshot held packed, in snapshot order (#725).
#[derive(Debug, Clone, Default)]
pub struct PackedSnapshot {
    /// Nodes in id order.
    pub nodes: Vec<PackedNode>,
    /// Edges in `(src, dst, label)` order.
    pub edges: Vec<PackedEdge>,
}

impl crate::ExpectedSnapshot for PackedSnapshot {
    fn node_matches(
        &self,
        index: usize,
        id: &str,
        label: &str,
        props: &serde_json::Value,
    ) -> Result<bool, GraphError> {
        let Some(node) = self.nodes.get(index) else {
            return Ok(false);
        };
        Ok(id == node.id && label == node.label && *props == node.unpack()?.props)
    }
    fn node_count(&self) -> usize {
        self.nodes.len()
    }
    fn edge_matches(
        &self,
        index: usize,
        (src, dst, label): (&str, &str, &str),
        props: &serde_json::Value,
    ) -> Result<bool, GraphError> {
        let Some(edge) = self.edges.get(index) else {
            return Ok(false);
        };
        Ok(src == edge.src
            && dst == edge.dst
            && label == edge.label
            && *props == edge.unpack()?.props)
    }
    fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{edge_digest, node_digest};
    use serde_json::json;

    fn hostile() -> serde_json::Value {
        let mut props = json!({
            "zeta": [3, 1, 2, {"b": 1, "a": [true, null]}],
            "alpha": {"y": -0.0, "x": 1.5e300, "w": 0.1, "v": u64::MAX, "u": i64::MIN},
            "text": "é\"\\\n\u{0}\u{2028}😀",
            "empty": {"": [], "o": {}},
        });
        props["prov"] = json!({
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "content_hash": "a".repeat(64),
            "extractor_id": "t0.adapter-ts",
            "evidence": [{"repo": "local/r", "path": "src/a.ts", "byte_start": 0,
                          "byte_end": 9, "commit_sha": "c".repeat(40)}],
        });
        props
    }

    #[test]
    fn packed_facts_round_trip_exactly_and_keep_publication_bytes() {
        // AC-0526: nested order, arrays, numbers and escapes survive packing;
        // the bytes are publication's and the complete-fact digests agree.
        let node = Node {
            id: "sym:a".into(),
            label: "Symbol".into(),
            props: hostile(),
        };
        let packed = PackedNode::pack(node.clone()).unwrap();
        assert_eq!(packed.unpack().unwrap(), node);
        assert_eq!(
            packed.props_json(),
            serde_json::to_string(&node.props).unwrap()
        );
        assert_eq!(
            node_digest(&packed.unpack().unwrap()).unwrap(),
            node_digest(&node).unwrap()
        );

        let edge = Edge {
            src: "sym:a".into(),
            dst: "sym:b".into(),
            label: "CALLS".into(),
            props: hostile(),
        };
        let packed = PackedEdge::pack(edge.clone()).unwrap();
        assert_eq!(packed.unpack().unwrap(), edge);
        assert_eq!(
            packed.props_json(),
            serde_json::to_string(&edge.props).unwrap()
        );
        assert_eq!(
            edge_digest(&packed.unpack().unwrap()).unwrap(),
            edge_digest(&edge).unwrap()
        );

        for scalar in [
            json!(null),
            json!(0),
            json!(-1),
            json!(2.5),
            json!("s"),
            json!([]),
        ] {
            let node = Node {
                id: "n".into(),
                label: "L".into(),
                props: scalar,
            };
            assert_eq!(
                PackedNode::pack(node.clone()).unwrap().unpack().unwrap(),
                node
            );
        }

        // Every finite f64 keeps its exact bits through pack, unpack and a
        // stored row, including values the default fast float parser
        // misrounds (-2.481203691041865e275 parses one ulp off).
        let mut floats = vec![
            51.248178375505404,
            -2.481203691041865e275,
            1.0715660391465826e-75,
        ];
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        while floats.len() < 20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let float = f64::from_bits(state);
            if float.is_finite() {
                floats.push(float);
            }
        }
        let mut store = crate::SqliteGraphStore::open_in_memory().unwrap();
        for (index, float) in floats.iter().enumerate() {
            let node = Node {
                id: format!("f{index}"),
                label: "L".into(),
                props: json!({ "x": float }),
            };
            let unpacked = PackedNode::pack(node.clone()).unwrap().unpack().unwrap();
            assert_eq!(
                unpacked.props["x"].as_f64().map(f64::to_bits),
                Some(float.to_bits()),
                "{float:e}"
            );
            assert_eq!(unpacked, node);
            crate::GraphStore::put_node(&mut store, &node).unwrap();
        }
        let (stored, _) = store.read_snapshot().unwrap();
        for node in stored {
            let index: usize = node.id[1..].parse().unwrap();
            assert_eq!(
                node.props["x"].as_f64().map(f64::to_bits),
                Some(floats[index].to_bits())
            );
        }

        // The depth boundary: 127 nested containers (arrays and objects)
        // pack and unpack exactly; 128 are refused, so every packed fact
        // unpacks.
        let nested = |containers: usize| {
            let mut props = json!(0);
            for level in 0..containers {
                props = if level % 2 == 0 {
                    json!([props])
                } else {
                    json!({ "k": props })
                };
            }
            props
        };
        assert!(
            serde_json::from_str::<serde_json::Value>(
                &serde_json::to_string(&nested(MAX_PROPS_DEPTH + 1)).unwrap()
            )
            .is_err()
        );
        for (containers, accepted) in [(MAX_PROPS_DEPTH, true), (MAX_PROPS_DEPTH + 1, false)] {
            let node = Node {
                id: "deep".into(),
                label: "L".into(),
                props: nested(containers),
            };
            let edge = Edge {
                src: "a".into(),
                dst: "b".into(),
                label: "L".into(),
                props: nested(containers),
            };
            match (
                PackedNode::pack(node.clone()),
                PackedEdge::pack(edge.clone()),
            ) {
                (Ok(n), Ok(e)) if accepted => {
                    assert_eq!(n.unpack().unwrap(), node);
                    assert_eq!(e.unpack().unwrap(), edge);
                }
                (Err(GraphError::FactStore(_)), Err(GraphError::FactStore(_))) if !accepted => {}
                _ => panic!("{containers} nested containers: accepted should be {accepted}"),
            }
        }
    }

    #[test]
    fn packed_snapshots_and_upserts_publish_the_same_rows() {
        // AC-0528: a packed snapshot reads the same facts, validates a
        // conditional publish like the unpacked one (stale is refused), and
        // packed upserts store the rows unpacked upserts store.
        use crate::{GraphPatch, GraphStore, SqliteGraphStore};
        let seeded = || {
            let mut store = SqliteGraphStore::open_in_memory().unwrap();
            for (id, n) in [("b", 2), ("a", 1)] {
                store
                    .put_node(&Node {
                        id: id.into(),
                        label: "Symbol".into(),
                        props: json!({"n": n, "prov": hostile()["prov"].clone()}),
                    })
                    .unwrap();
            }
            store
                .put_edge(&Edge {
                    src: "a".into(),
                    dst: "b".into(),
                    label: "CALLS".into(),
                    props: hostile(),
                })
                .unwrap();
            store
        };
        let new_node = Node {
            id: "c".into(),
            label: "Symbol".into(),
            props: hostile(),
        };
        let new_edge = Edge {
            src: "b".into(),
            dst: "c".into(),
            label: "CALLS".into(),
            props: json!({"z": 1, "a": [2.5, -1]}),
        };

        let mut plain = seeded();
        let expected = plain.read_snapshot().unwrap();
        let unpacked = GraphPatch {
            upsert_nodes: vec![new_node.clone()],
            upsert_edges: vec![new_edge.clone()],
            ..GraphPatch::default()
        };
        assert!(
            plain
                .apply_patch_if_snapshot_matches(&expected, &unpacked)
                .unwrap()
        );

        let mut packed_store = seeded();
        let snapshot = packed_store.read_packed_snapshot().unwrap();
        let unpacked_snapshot = (
            snapshot
                .nodes
                .iter()
                .map(|n| n.unpack().unwrap())
                .collect::<Vec<_>>(),
            snapshot
                .edges
                .iter()
                .map(|e| e.unpack().unwrap())
                .collect::<Vec<_>>(),
        );
        assert_eq!(unpacked_snapshot, expected);
        let packed = GraphPatch {
            upsert_packed_nodes: vec![PackedNode::pack(new_node).unwrap()],
            upsert_packed_edges: vec![PackedEdge::pack(new_edge).unwrap()],
            ..GraphPatch::default()
        };
        assert!(
            packed_store
                .apply_patch_if_snapshot_matches(&snapshot, &packed)
                .unwrap()
        );
        assert_eq!(
            packed_store.read_snapshot().unwrap(),
            plain.read_snapshot().unwrap()
        );
        let rows = |store: &SqliteGraphStore| {
            store
                .conn
                .prepare("SELECT id, props FROM nodes ORDER BY id")
                .unwrap()
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(rows(&packed_store), rows(&plain));

        // The snapshot is now stale: the packed check refuses it unchanged.
        let before = packed_store.read_snapshot().unwrap();
        assert!(
            !packed_store
                .apply_patch_if_snapshot_matches(&snapshot, &packed)
                .unwrap()
        );
        assert_eq!(packed_store.read_snapshot().unwrap(), before);

        // A mixed patch: an unpacked edge may reference a packed new node
        // and a packed edge an unpacked one.
        let mut mixed = seeded();
        let expected = mixed.read_packed_snapshot().unwrap();
        let node = |id: &str| Node {
            id: id.into(),
            label: "Symbol".into(),
            props: json!({}),
        };
        let link = |src: &str, dst: &str| Edge {
            src: src.into(),
            dst: dst.into(),
            label: "CALLS".into(),
            props: json!({}),
        };
        let patch = GraphPatch {
            upsert_nodes: vec![node("u")],
            upsert_edges: vec![link("a", "p")],
            upsert_packed_nodes: vec![PackedNode::pack(node("p")).unwrap()],
            upsert_packed_edges: vec![PackedEdge::pack(link("p", "u")).unwrap()],
            ..GraphPatch::default()
        };
        assert!(
            mixed
                .apply_patch_if_snapshot_matches(&expected, &patch)
                .unwrap()
        );
        let (_, edges) = mixed.read_snapshot().unwrap();
        assert!(edges.iter().any(|e| e.src == "a" && e.dst == "p"));
        assert!(edges.iter().any(|e| e.src == "p" && e.dst == "u"));
    }
}
