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
    }
}
