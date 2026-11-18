//! Complete-graph streaming under the same admission as the bounded snapshot,
//! so a caller can derive a complete-graph identity without retaining the
//! graph (#1602). Retained memory is one decoded row, not the estate.
use super::{
    ByteCounter, CanonicalEdge, CanonicalNode, JsonBudget, field, parse_properties, preflight_rows,
    preflight_schema,
};
use crate::scoped::ScopedGraphReader;
use crate::{Edge, GraphError, Node, SnapshotBoundsError, SnapshotReadLimits, SqliteGraphStore};
use rusqlite::Connection;

/// One complete-graph row, delivered in context identity order: every node
/// by `id`, then every edge by `(src, label, dst)`, all in SQLite BINARY order.
#[derive(Debug, Clone, PartialEq)]
pub enum SnapshotRow {
    /// A stored node.
    Node(Node),
    /// A stored edge, after every node.
    Edge(Edge),
}

/// Counts of a complete streamed graph that passed admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamedSnapshot {
    /// Nodes delivered.
    pub nodes: usize,
    /// Edges delivered.
    pub edges: usize,
}

impl SqliteGraphStore {
    /// Stream the complete graph in one SQLite revision under `limits`.
    ///
    /// It admits exactly the graphs [`Self::read_snapshot_bounded`] admits:
    /// the same row, raw byte, depth, value and canonical byte limits. It
    /// retains one row at a time. The visitor may already have seen rows when
    /// a later row or the final canonical byte total is refused, so a caller
    /// must discard what it built on any error. A graph refused for more than
    /// one reason may report a different one of those reasons.
    pub fn visit_snapshot_bounded<E: From<GraphError>>(
        &self,
        limits: SnapshotReadLimits,
        visit: impl FnMut(SnapshotRow) -> Result<(), E>,
    ) -> Result<StreamedSnapshot, E> {
        limits.validate().map_err(E::from)?;
        let transaction = self
            .conn
            .unchecked_transaction()
            .map_err(GraphError::from)
            .map_err(E::from)?;
        let streamed = visit_rows(&transaction, limits, visit)?;
        transaction
            .commit()
            .map_err(GraphError::from)
            .map_err(E::from)?;
        Ok(streamed)
    }
}

impl ScopedGraphReader<'_> {
    /// Stream the complete graph inside this reader's pinned transaction, so a
    /// scoped acquisition and a complete-graph identity share one revision.
    /// `limits` apply to this stream alone and do not charge the scoped budgets.
    pub fn visit_complete_snapshot<E: From<GraphError>>(
        &self,
        limits: SnapshotReadLimits,
        visit: impl FnMut(SnapshotRow) -> Result<(), E>,
    ) -> Result<StreamedSnapshot, E> {
        limits.validate().map_err(E::from)?;
        visit_rows(self.connection(), limits, visit)
    }
}

fn visit_rows<E: From<GraphError>>(
    connection: &Connection,
    limits: SnapshotReadLimits,
    mut visit: impl FnMut(SnapshotRow) -> Result<(), E>,
) -> Result<StreamedSnapshot, E> {
    preflight_schema(connection).map_err(E::from)?;
    preflight_rows(connection, limits).map_err(E::from)?;
    let mut budget = JsonBudget::new(limits);
    let enter = |budget: &mut JsonBudget, depth| {
        budget
            .enter(depth)
            .map_err(|error| E::from(GraphError::from(error)))
    };
    enter(&mut budget, 1)?; // complete graph tuple
    enter(&mut budget, 2)?; // nodes array
    enter(&mut budget, 2)?; // edges array
    // The complete tuple `[[n,...],[e,...]]`: brackets plus one comma between
    // the two arrays; each further row adds its own comma. Exceeding is
    // reported only after every row decoded, as the materialized read does.
    let mut canonical = ByteCounter {
        remaining: limits.max_bytes,
    };
    let mut canonical_ok = count(&mut canonical, b"[[],[]]");
    let mut streamed = StreamedSnapshot { nodes: 0, edges: 0 };
    {
        let mut statement = connection
            .prepare("SELECT id, label, props FROM nodes ORDER BY id COLLATE BINARY")
            .map_err(GraphError::from)
            .map_err(E::from)?;
        let mut rows = statement
            .query([])
            .map_err(GraphError::from)
            .map_err(E::from)?;
        while let Some(row) = rows.next().map_err(GraphError::from).map_err(E::from)? {
            enter(&mut budget, 3)?;
            enter(&mut budget, 4)?; // id
            enter(&mut budget, 4)?; // label
            let id = field(row, 0).map_err(E::from)?;
            let label = field(row, 1).map_err(E::from)?;
            let props =
                parse_properties(&field(row, 2).map_err(E::from)?, &mut budget).map_err(E::from)?;
            let node = Node { id, label, props };
            canonical_ok = canonical_ok
                && (streamed.nodes == 0 || count(&mut canonical, b","))
                && serde_json::to_writer(&mut canonical, &CanonicalNode(&node)).is_ok();
            streamed.nodes += 1;
            visit(SnapshotRow::Node(node))?;
        }
    }
    {
        let mut statement = connection
            .prepare(
                "SELECT src, dst, label, props FROM edges \
                 ORDER BY src COLLATE BINARY, label COLLATE BINARY, dst COLLATE BINARY",
            )
            .map_err(GraphError::from)
            .map_err(E::from)?;
        let mut rows = statement
            .query([])
            .map_err(GraphError::from)
            .map_err(E::from)?;
        while let Some(row) = rows.next().map_err(GraphError::from).map_err(E::from)? {
            enter(&mut budget, 3)?;
            enter(&mut budget, 4)?; // src
            enter(&mut budget, 4)?; // dst
            enter(&mut budget, 4)?; // label
            let src = field(row, 0).map_err(E::from)?;
            let dst = field(row, 1).map_err(E::from)?;
            let label = field(row, 2).map_err(E::from)?;
            let props =
                parse_properties(&field(row, 3).map_err(E::from)?, &mut budget).map_err(E::from)?;
            let edge = Edge {
                src,
                dst,
                label,
                props,
            };
            canonical_ok = canonical_ok
                && (streamed.edges == 0 || count(&mut canonical, b","))
                && serde_json::to_writer(&mut canonical, &CanonicalEdge(&edge)).is_ok();
            streamed.edges += 1;
            visit(SnapshotRow::Edge(edge))?;
        }
    }
    if !canonical_ok {
        return Err(E::from(SnapshotBoundsError::CanonicalByteLimit.into()));
    }
    Ok(streamed)
}

fn count(counter: &mut ByteCounter, bytes: &[u8]) -> bool {
    std::io::Write::write_all(counter, bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GraphStore;
    use serde_json::json;

    fn store() -> SqliteGraphStore {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for (id, props) in [
            ("b", json!({"z": 1, "a": [1.5, "x\"y", {"k": null}]})),
            ("a", json!({"name": "caf\u{e9}"})),
            ("c", serde_json::Value::Null),
        ] {
            store
                .put_node(&Node {
                    id: id.into(),
                    label: "Symbol".into(),
                    props,
                })
                .unwrap();
        }
        // Primary-key order (src,dst,label) differs from identity order
        // (src,label,dst) for the two edges from `a`.
        for (src, dst, label) in [("a", "b", "Z"), ("a", "c", "A"), ("b", "a", "CALLS")] {
            store
                .put_edge(&Edge {
                    src: src.into(),
                    dst: dst.into(),
                    label: label.into(),
                    props: json!({"w": dst}),
                })
                .unwrap();
        }
        store
    }

    fn collect(
        store: &SqliteGraphStore,
        limits: SnapshotReadLimits,
    ) -> Result<Vec<SnapshotRow>, GraphError> {
        let mut rows = Vec::new();
        store.visit_snapshot_bounded(limits, |row| {
            rows.push(row);
            Ok::<_, GraphError>(())
        })?;
        Ok(rows)
    }

    // T-0735 (#1602): the stream delivers the materialized snapshot's facts,
    // nodes by id then edges by (src, label, dst).
    #[test]
    fn stream_delivers_every_fact_in_identity_order() {
        let store = store();
        let (nodes, mut edges) = store
            .read_snapshot_bounded(SnapshotReadLimits::default())
            .unwrap();
        edges.sort_by(|l, r| (&l.src, &l.label, &l.dst).cmp(&(&r.src, &r.label, &r.dst)));
        let expected: Vec<_> = nodes
            .into_iter()
            .map(SnapshotRow::Node)
            .chain(edges.into_iter().map(SnapshotRow::Edge))
            .collect();
        assert_eq!(
            collect(&store, SnapshotReadLimits::default()).unwrap(),
            expected
        );
        let empty = SqliteGraphStore::open_in_memory().unwrap();
        assert!(
            collect(&empty, SnapshotReadLimits::default())
                .unwrap()
                .is_empty()
        );
    }

    // T-0735 (#1602): the stream admits exactly the graphs the materialized
    // bounded read admits, at every row, byte and value boundary.
    #[test]
    fn stream_admission_matches_the_materialized_bounded_read() {
        let store = store();
        let base = SnapshotReadLimits::default();
        let admits = |limits: SnapshotReadLimits| {
            (
                store.read_snapshot_bounded(limits).is_ok(),
                collect(&store, limits).is_ok(),
            )
        };
        let mut boundaries = 0;
        for max_rows in 0..10 {
            let (read, streamed) = admits(SnapshotReadLimits { max_rows, ..base });
            assert_eq!(read, streamed, "max_rows {max_rows}");
        }
        for max_bytes in 1..400 {
            let (read, streamed) = admits(SnapshotReadLimits { max_bytes, ..base });
            assert_eq!(read, streamed, "max_bytes {max_bytes}");
            boundaries += usize::from(read);
        }
        for max_values in 1..80 {
            let (read, streamed) = admits(SnapshotReadLimits { max_values, ..base });
            assert_eq!(read, streamed, "max_values {max_values}");
        }
        for max_depth in 1..8 {
            let (read, streamed) = admits(SnapshotReadLimits { max_depth, ..base });
            assert_eq!(read, streamed, "max_depth {max_depth}");
        }
        // The byte sweep crossed the canonical boundary, not only raw preflight.
        assert!(boundaries > 0 && boundaries < 399);
        let canonical =
            crate::bounded::canonical_graph_bytes(&store.read_snapshot().unwrap(), usize::MAX)
                .unwrap();
        let at = SnapshotReadLimits {
            max_bytes: canonical,
            ..base
        };
        assert!(collect(&store, at).is_ok());
        assert!(matches!(
            collect(
                &store,
                SnapshotReadLimits {
                    max_bytes: canonical - 1,
                    ..base
                }
            ),
            Err(GraphError::SnapshotBounds(
                SnapshotBoundsError::CanonicalByteLimit
            ))
        ));
    }

    // T-0735 (#1602): inside a scoped reader the stream shares its revision
    // and does not charge the scoped budgets.
    #[test]
    fn scoped_reader_streams_the_complete_graph_in_its_pinned_revision() {
        let store = store();
        let revision = store.read_revision().unwrap();
        let expected = collect(&store, SnapshotReadLimits::default()).unwrap();
        let (rows, usage) = store
            .with_scoped_reader(&revision, SnapshotReadLimits::default(), |reader| {
                let mut rows = Vec::new();
                reader.visit_complete_snapshot(SnapshotReadLimits::default(), |row| {
                    rows.push(row);
                    Ok::<_, GraphError>(())
                })?;
                Ok::<_, GraphError>((rows, reader.usage()))
            })
            .unwrap();
        assert_eq!(rows, expected);
        assert_eq!(usage, crate::scoped::ScopedReadUsage::default());
    }
}
