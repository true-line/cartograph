//! Complete induced neighborhoods acquired through the existing bounded reader.
use super::*;
use std::collections::BTreeMap;

/// The complete induced facts of one scoped neighborhood, not a whole graph.
pub type NeighborhoodGraph = (Vec<Node>, Vec<Edge>);

impl ScopedGraphReader<'_> {
    /// Acquire the undirected induced neighborhood at the pinned revision.
    /// Missing anchors return None. Dangling edges never manufacture nodes.
    /// Every probe/page consumes this reader's cumulative budgets; exhaustion
    /// returns an error, never a truncated neighborhood. This is not a whole
    /// graph snapshot and must not be assigned a complete-graph identity.
    pub fn neighborhood(
        &mut self,
        anchor: &str,
        hops: usize,
    ) -> Result<Option<NeighborhoodGraph>, GraphError> {
        if hops == 0 {
            return Err(SnapshotBoundsError::InvalidLimits.into());
        }
        let Some(anchor_node) = self.node(anchor)? else {
            return Ok(None);
        };
        let mut nodes = BTreeMap::from([(anchor.to_owned(), anchor_node)]);
        let mut edges = BTreeMap::new();
        let mut frontier = BTreeSet::from([anchor.to_owned()]);
        for depth in 0..=hops {
            let mut next = BTreeSet::new();
            for root in frontier {
                let mut outgoing = None;
                loop {
                    let page = self.outgoing(&root, outgoing.as_ref(), self.limits().max_rows)?;
                    for edge in page.edges {
                        if depth < hops
                            && !nodes.contains_key(&edge.dst)
                            && let Some(node) = self.node(&edge.dst)?
                        {
                            next.insert(node.id.clone());
                            nodes.insert(node.id.clone(), node);
                        }
                        edges.insert(
                            (edge.src.clone(), edge.dst.clone(), edge.label.clone()),
                            edge,
                        );
                    }
                    match page.remaining {
                        Remaining::Exhausted => break,
                        Remaining::More => outgoing = page.next_outgoing,
                    }
                }
                // Outgoing edges from every visited node suffice for the final
                // induced edge set; incoming reads only discover the next shell.
                if depth < hops {
                    let mut incoming = None;
                    loop {
                        let page = self.incoming(&root, incoming, self.limits().max_rows)?;
                        for edge in page.edges {
                            if !nodes.contains_key(&edge.src)
                                && let Some(node) = self.node(&edge.src)?
                            {
                                next.insert(node.id.clone());
                                nodes.insert(node.id.clone(), node);
                            }
                        }
                        match page.remaining {
                            Remaining::Exhausted => break,
                            Remaining::More => incoming = page.next_incoming,
                        }
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        edges.retain(|(src, dst, _), _| nodes.contains_key(src) && nodes.contains_key(dst));
        Ok(Some((
            nodes.into_values().collect(),
            edges.into_values().collect(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> SqliteGraphStore {
        let store = SqliteGraphStore::open_in_memory().unwrap();
        store.conn.execute_batch("PRAGMA foreign_keys=OFF; INSERT INTO nodes VALUES ('a','Symbol','{}'),('b','Symbol','{}'),('c','Symbol','{}'),('far','Symbol','{}'); INSERT INTO edges VALUES ('a','b','CALLS','{}'),('c','a','CALLS','{}'),('b','c','CALLS','{}'),('b','far','CALLS','{}'),('a','missing','CALLS','{}');").unwrap();
        store
    }

    // #1602: an undirected shell includes induced cross edges, not dangling nodes.
    #[test]
    fn neighborhood_is_induced_and_revision_bound() {
        let store = fixture();
        let revision = store.read_revision().unwrap();
        let graph = store
            .with_scoped_reader(&revision, SnapshotReadLimits::default(), |r| {
                r.neighborhood("a", 1)
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            graph.0.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(
            graph
                .1
                .iter()
                .map(|e| (e.src.as_str(), e.dst.as_str()))
                .collect::<Vec<_>>(),
            [("a", "b"), ("b", "c"), ("c", "a")]
        );
        store
            .conn
            .execute("INSERT INTO nodes VALUES ('changed','Symbol','{}')", [])
            .unwrap();
        assert!(matches!(
            store.with_scoped_reader(&revision, SnapshotReadLimits::default(), |r| r
                .neighborhood("a", 1)),
            Err(GraphError::Generation(_))
        ));
    }

    #[test]
    fn neighborhood_refuses_exhaustion_instead_of_truncating() {
        let store = fixture();
        let revision = store.read_revision().unwrap();
        let limits = SnapshotReadLimits {
            max_rows: 2,
            ..Default::default()
        };
        assert!(matches!(
            store.with_scoped_reader(&revision, limits, |r| r.neighborhood("a", 1)),
            Err(GraphError::SnapshotBounds(SnapshotBoundsError::RowLimit))
        ));
    }

    #[test]
    fn small_neighborhood_survives_large_unrelated_estate() {
        let mut store = fixture();
        let tx = store.conn.transaction().unwrap();
        {
            let mut insert = tx
                .prepare("INSERT INTO nodes VALUES (?1,'Symbol','{}')")
                .unwrap();
            for i in 0..50_001 {
                insert.execute([format!("unrelated:{i}")]).unwrap();
            }
        }
        tx.commit().unwrap();
        assert!(
            store
                .read_snapshot_bounded(SnapshotReadLimits::default())
                .is_err()
        );
        let revision = store.read_revision().unwrap();
        let graph = store
            .with_scoped_reader(&revision, SnapshotReadLimits::default(), |r| {
                r.neighborhood("a", 1)
            })
            .unwrap()
            .unwrap();
        assert_eq!((graph.0.len(), graph.1.len()), (3, 3));
    }
}
