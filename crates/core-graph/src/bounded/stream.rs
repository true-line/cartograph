//! Complete-graph streaming under the same admission as the bounded snapshot,
//! so a caller can derive a complete-graph identity without retaining the
//! graph (#1602). Retained memory is one decoded row, not the estate.
use super::{
    ByteCounter, CanonicalEdge, CanonicalNode, JsonBudget, field, parse_properties, preflight_rows,
    preflight_schema,
};
use crate::scoped::ScopedGraphReader;
use crate::{
    Edge, GraphError, IdentityPassLimits, Node, SnapshotBoundsError, SnapshotReadLimits,
    SqliteGraphStore,
};
use rusqlite::Connection;
use serde_json::Value;

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
    /// Under [`IdentityPassLimits::uniform`] (what a plain
    /// [`SnapshotReadLimits`] converts to) it admits exactly the graphs
    /// [`Self::read_snapshot_bounded`] admits: the same row, raw byte, depth,
    /// value and canonical byte limits. `limits.complete` is that aggregate
    /// admission and `limits.row` additionally caps the one row retained at a
    /// time (raw bytes before its body is copied, property values and depth
    /// while it decodes). The visitor may already have seen rows when
    /// a later row or the final canonical byte total is refused, so a caller
    /// must discard what it built on any error. A graph refused for more than
    /// one reason may report a different one of those reasons.
    pub fn visit_snapshot_bounded<E: From<GraphError>>(
        &self,
        limits: impl Into<IdentityPassLimits>,
        visit: impl FnMut(SnapshotRow) -> Result<(), E>,
    ) -> Result<StreamedSnapshot, E> {
        let limits = limits.into();
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
    /// `limits` apply to this stream alone and do not charge the scoped budgets;
    /// [`IdentityPassLimits::for_retained`] with [`Self::limits`] is the
    /// preparation choice (ADR-0052 amendment 2026-11-18).
    pub fn visit_complete_snapshot<E: From<GraphError>>(
        &self,
        limits: impl Into<IdentityPassLimits>,
        visit: impl FnMut(SnapshotRow) -> Result<(), E>,
    ) -> Result<StreamedSnapshot, E> {
        let limits = limits.into();
        limits.validate().map_err(E::from)?;
        visit_rows(self.connection(), limits, visit)
    }
}

/// Walks the primary-key index; no sort. The last column is the row's raw
/// stored bytes, checked against the retained-row cap before any body is copied.
const NODES_IN_IDENTITY_ORDER: &str = "SELECT id, label, props, \
     length(CAST(id AS BLOB)) + length(CAST(label AS BLOB)) + length(CAST(props AS BLOB)) \
     FROM nodes ORDER BY id COLLATE BINARY";
/// The primary key orders edges (src, dst, label), so identity order would
/// otherwise sort in a temp b-tree, held in memory under `temp_store=MEMORY`
/// and as large as the biggest source's fan-out. `INDEXED BY` walks the
/// (src, label, dst) index instead and fails closed if it is missing.
const EDGES_IN_IDENTITY_ORDER: &str = "SELECT src, dst, label, props, \
     length(CAST(src AS BLOB)) + length(CAST(dst AS BLOB)) + length(CAST(label AS BLOB)) \
     + length(CAST(props AS BLOB)) FROM edges \
     INDEXED BY idx_edges_src_label_dst \
     ORDER BY src COLLATE BINARY, label COLLATE BINARY, dst COLLATE BINARY";

fn visit_rows<E: From<GraphError>>(
    connection: &Connection,
    limits: IdentityPassLimits,
    mut visit: impl FnMut(SnapshotRow) -> Result<(), E>,
) -> Result<StreamedSnapshot, E> {
    let IdentityPassLimits {
        complete: limits,
        row: row_limits,
    } = limits;
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
            .prepare(NODES_IN_IDENTITY_ORDER)
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
            admit_row(row, 3, row_limits).map_err(E::from)?;
            let id = field(row, 0).map_err(E::from)?;
            let label = field(row, 1).map_err(E::from)?;
            let props =
                decode_row_properties(&field(row, 2).map_err(E::from)?, &mut budget, row_limits)
                    .map_err(E::from)?;
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
            .prepare(EDGES_IN_IDENTITY_ORDER)
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
            admit_row(row, 4, row_limits).map_err(E::from)?;
            let src = field(row, 0).map_err(E::from)?;
            let dst = field(row, 1).map_err(E::from)?;
            let label = field(row, 2).map_err(E::from)?;
            let props =
                decode_row_properties(&field(row, 3).map_err(E::from)?, &mut budget, row_limits)
                    .map_err(E::from)?;
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

/// Refuse a row whose raw stored bytes exceed the retained-row cap, before
/// its body is copied out of SQLite.
fn admit_row(
    row: &rusqlite::Row<'_>,
    length_column: usize,
    row_limits: SnapshotReadLimits,
) -> Result<(), GraphError> {
    let length = row
        .get::<_, Option<i64>>(length_column)
        .map_err(|_| SnapshotBoundsError::InvalidRow)?
        .and_then(|length| usize::try_from(length).ok())
        .ok_or(SnapshotBoundsError::InvalidRow)?;
    if length > row_limits.max_bytes {
        return Err(SnapshotBoundsError::SingleRowByteLimit.into());
    }
    Ok(())
}

/// Decode one row's properties against the smaller of the aggregate values
/// left and the retained-row value cap, then charge the aggregate. A refusal
/// is reported as the aggregate limit unless the row cap was the binding one,
/// so uniform limits keep exactly the materialized read's errors.
fn decode_row_properties(
    raw: &str,
    budget: &mut JsonBudget,
    row_limits: SnapshotReadLimits,
) -> Result<Value, GraphError> {
    let available = budget.remaining;
    let cap = row_limits.max_values.min(available);
    let mut row_budget = JsonBudget {
        limits: SnapshotReadLimits {
            max_values: cap,
            max_depth: row_limits.max_depth.min(budget.limits.max_depth),
            ..budget.limits
        },
        remaining: cap,
        failure: None,
    };
    match parse_properties(raw, &mut row_budget) {
        Ok(value) => {
            budget.remaining -= row_budget.consumed_values();
            Ok(value)
        }
        Err(GraphError::SnapshotBounds(SnapshotBoundsError::JsonValueLimit)) if cap < available => {
            Err(SnapshotBoundsError::SingleRowValueLimit.into())
        }
        Err(error) => Err(error),
    }
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

    // T-0775 (#1602, ADR-0052 amendment 2026-11-18): the identity pass has
    // its own aggregate caps; the retained-row limits cap only the one row
    // held at a time, and each refusal names the cap that was hit.
    #[test]
    fn identity_pass_caps_are_separate_from_the_retained_row_caps() {
        let store = store();
        let expected = collect(&store, SnapshotReadLimits::default()).unwrap();
        let retained = SnapshotReadLimits {
            max_rows: 1,
            max_bytes: 64,
            max_values: 16,
            ..SnapshotReadLimits::default()
        };
        let stream = |limits: IdentityPassLimits| {
            let mut rows = Vec::new();
            store
                .visit_snapshot_bounded(limits, |row| {
                    rows.push(row);
                    Ok::<_, GraphError>(())
                })
                .map(|_| rows)
        };
        // Six rows exceed the retained row count, yet the pass admits them.
        assert!(matches!(
            store.read_snapshot_bounded(retained),
            Err(GraphError::SnapshotBounds(SnapshotBoundsError::RowLimit))
        ));
        let preparation = IdentityPassLimits::for_retained(retained);
        assert_eq!(preparation.complete, SnapshotReadLimits::investigation());
        assert_eq!(preparation.complete.max_rows, 10_000_000);
        assert_eq!(preparation.complete.max_bytes, 4 * 1024 * 1024 * 1024);
        assert_eq!(stream(preparation).unwrap(), expected);
        // One row over the retained byte or value cap is refused as that row.
        for (row, refusal) in [
            (
                SnapshotReadLimits {
                    max_bytes: 1,
                    ..retained
                },
                SnapshotBoundsError::SingleRowByteLimit,
            ),
            (
                SnapshotReadLimits {
                    max_values: 1,
                    ..retained
                },
                SnapshotBoundsError::SingleRowValueLimit,
            ),
            (
                SnapshotReadLimits {
                    max_depth: 4,
                    ..retained
                },
                SnapshotBoundsError::JsonDepthLimit,
            ),
        ] {
            assert!(
                matches!(
                    stream(IdentityPassLimits::for_retained(row)),
                    Err(GraphError::SnapshotBounds(error)) if error == refusal
                ),
                "{refusal:?}"
            );
        }
        // The aggregate caps still apply to the whole stream and keep their
        // own errors, even when the row cap is looser.
        let total_values = (1..200)
            .find(|&max_values| {
                stream(
                    SnapshotReadLimits {
                        max_values,
                        ..SnapshotReadLimits::default()
                    }
                    .into(),
                )
                .is_ok()
            })
            .unwrap();
        for (complete, refusal) in [
            (
                SnapshotReadLimits {
                    max_rows: 5,
                    ..SnapshotReadLimits::default()
                },
                SnapshotBoundsError::RowLimit,
            ),
            (
                SnapshotReadLimits {
                    max_values: total_values - 1,
                    ..SnapshotReadLimits::default()
                },
                SnapshotBoundsError::JsonValueLimit,
            ),
        ] {
            let limits = IdentityPassLimits {
                complete,
                row: SnapshotReadLimits::default(),
            };
            assert!(
                matches!(
                    stream(limits),
                    Err(GraphError::SnapshotBounds(error)) if error == refusal
                ),
                "{refusal:?}"
            );
        }
        // Invalid row limits are refused like invalid aggregate limits.
        assert!(matches!(
            stream(IdentityPassLimits::for_retained(SnapshotReadLimits {
                max_values: 0,
                ..retained
            })),
            Err(GraphError::SnapshotBounds(
                SnapshotBoundsError::InvalidLimits
            ))
        ));
    }

    fn plan(connection: &Connection, sql: &str) -> Vec<String> {
        let mut statement = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    // T-0735 (#1602): both stream queries walk an index in identity order;
    // neither sorts in a temp b-tree, so memory stays one row per fact.
    #[test]
    fn stream_queries_use_indexes_without_a_temp_b_tree() {
        let store = store();
        for (sql, index) in [
            (NODES_IN_IDENTITY_ORDER, "sqlite_autoindex_nodes_1"),
            (EDGES_IN_IDENTITY_ORDER, "idx_edges_src_label_dst"),
        ] {
            let plan = plan(&store.conn, sql);
            assert!(
                plan.iter().all(|step| !step.contains("TEMP B-TREE")),
                "{plan:?}"
            );
            assert!(plan.iter().any(|step| step.contains(index)), "{plan:?}");
        }
    }

    // T-0735 (#1602): a store created before the index gains it on open with
    // the same facts; without it the stream refuses instead of sorting.
    #[test]
    fn existing_stores_gain_the_identity_order_index_and_keep_their_facts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let before = {
            let mut store = SqliteGraphStore::open(&path).unwrap();
            let fresh = self::store();
            let (nodes, edges) = fresh.read_snapshot().unwrap();
            for node in &nodes {
                store.put_node(node).unwrap();
            }
            for edge in &edges {
                store.put_edge(edge).unwrap();
            }
            store
                .conn
                .execute_batch("DROP INDEX idx_edges_src_label_dst")
                .unwrap();
            assert!(collect(&store, SnapshotReadLimits::default()).is_err());
            store.read_snapshot().unwrap()
        };
        let reopened = SqliteGraphStore::open(&path).unwrap();
        assert_eq!(reopened.read_snapshot().unwrap(), before);
        assert_eq!(
            collect(&reopened, SnapshotReadLimits::default())
                .unwrap()
                .len(),
            before.0.len() + before.1.len()
        );
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
