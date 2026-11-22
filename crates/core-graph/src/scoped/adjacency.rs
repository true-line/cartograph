//! Request-local indexed closure reads. Work limits include repeated/missing
//! probes; they bound logical acquisition, not SQLite VM instructions or RSS.
use super::*;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Cumulative work charged during this request, including repeated visits.
pub struct ScopedReadUsage {
    /// Decoded incident rows and exact/empty probes.
    pub visits: usize,
    /// Aggregate inspected UTF-8 SQL field bytes.
    pub raw_bytes: usize,
    /// Sum of canonical graph tuple encodings for acquired pages.
    pub canonical_bytes: usize,
    /// Independently capped by max_rows: one reserved existence probe per page.
    pub adjacency_probes: usize,
}
#[derive(Debug)]
/// One indexed incident page; labels are never filtered before charging.
pub struct AdjacencyPage {
    /// All admitted incident edges, including irrelevant producer labels.
    pub edges: Vec<Edge>,
    /// Whether this exact root/direction has another stored edge.
    pub remaining: Remaining,
    /// Exclusive outgoing key when another page exists.
    pub next_outgoing: Option<EdgeKey>,
    /// Signed revision-local rowid. INSERT OR REPLACE can change this position.
    pub next_incoming: Option<i64>,
}
/// Borrowed reader cannot outlive the callback's single deferred transaction.
pub struct ScopedGraphReader<'a> {
    connection: &'a Connection,
    revision: StoreRevision,
    limits: SnapshotReadLimits,
    usage: ScopedReadUsage,
    raw_remaining: usize,
    json: JsonBudget,
    pk: String,
}
impl SqliteGraphStore {
    /// Pin one deferred transaction for a complete bounded closure callback.
    /// Concurrent commits remain outside its snapshot; future old-revision
    /// requests fail. No transaction or reader reference can escape.
    pub fn with_scoped_reader<T, E: From<GraphError>>(
        &self,
        expected: &StoreRevision,
        limits: SnapshotReadLimits,
        callback: impl FnOnce(&mut ScopedGraphReader<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        validate_request(limits, Some(expected), false, &[]).map_err(E::from)?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(GraphError::from)
            .map_err(E::from)?;
        let revision = check_revision(&tx, Some(expected)).map_err(E::from)?;
        preflight_schema(&tx).map_err(E::from)?;
        validate_incoming(&tx).map_err(E::from)?;
        let pk = tx
            .query_row(
                "SELECT name FROM pragma_index_list('edges') WHERE origin='pk'",
                [],
                |r| r.get::<_, String>(0),
            )
            .map_err(GraphError::from)
            .map_err(E::from)?;
        let mut reader = ScopedGraphReader {
            connection: &tx,
            revision,
            limits,
            usage: ScopedReadUsage::default(),
            raw_remaining: limits.max_bytes,
            json: budget(limits).map_err(E::from)?,
            pk,
        };
        let result = callback(&mut reader)?;
        drop(reader);
        tx.commit().map_err(GraphError::from).map_err(E::from)?;
        Ok(result)
    }
}
fn validate_incoming(c: &Connection) -> Result<(), GraphError> {
    for sql in [
        "SELECT count(*)=1 AND coalesce(min(type='index' AND tbl_name='edges' AND sql IN ('CREATE INDEX idx_edges_dst ON edges(dst)','CREATE INDEX idx_edges_dst ON edges(dst COLLATE BINARY)')),0) FROM sqlite_schema WHERE name='idx_edges_dst'",
        "SELECT count(*)=1 AND coalesce(min(\"unique\"=0 AND origin='c' AND partial=0),0) FROM pragma_index_list('edges') WHERE name='idx_edges_dst'",
        "SELECT count(*)=2 AND sum(seqno=0 AND cid=1 AND name='dst' AND desc=0 AND coll='BINARY' AND key=1)=1 AND sum(seqno=1 AND cid=-1 AND name IS NULL AND desc=0 AND coll='BINARY' AND key=0)=1 FROM pragma_index_xinfo('idx_edges_dst')",
        "SELECT count(*)=1 AND coalesce(min(type='table' AND wr=0 AND strict=1),0) FROM pragma_table_list WHERE schema='main' AND name='edges'",
    ] {
        if !c.query_row(sql, [], |r| r.get::<_, bool>(0))? {
            return Err(SnapshotBoundsError::InvalidSchema.into());
        }
    }
    Ok(())
}
impl ScopedGraphReader<'_> {
    /// Revision pinned throughout this callback.
    pub fn revision(&self) -> &StoreRevision {
        &self.revision
    }
    /// Cumulative named acquisition charges.
    pub fn usage(&self) -> ScopedReadUsage {
        self.usage
    }
    /// Existing caller-selected graph ceilings, also applied cumulatively.
    pub fn limits(&self) -> SnapshotReadLimits {
        self.limits
    }
    /// The pinned transaction's connection, for crate-internal complete reads.
    pub(crate) fn connection(&self) -> &Connection {
        self.connection
    }
    pub(super) fn keys(&self, keys: &[&str]) -> Result<(), GraphError> {
        validate_request(self.limits, Some(&self.revision), false, keys)
    }
    pub(super) fn read(
        &mut self,
        kind: Kind,
        suffix: &str,
        p: &[SqlValue],
        n: usize,
    ) -> Result<(Vec<Node>, Vec<Edge>), GraphError> {
        let left = self.limits.max_rows - self.usage.visits;
        if n == 0 || n > left {
            return Err(SnapshotBoundsError::RowLimit.into());
        }
        let mut limits = self.limits;
        limits.max_rows = n;
        // Reserve before SQL; failed preflight/decode cannot be retried for free.
        self.usage.visits += n;
        let graph = read_rows(
            self.connection,
            kind,
            suffix,
            p,
            limits,
            &mut self.json,
            &mut self.raw_remaining,
        )?;
        self.usage.visits -= n - (graph.0.len() + graph.1.len()).max(1);
        self.usage.raw_bytes = self.limits.max_bytes - self.raw_remaining;
        let left = self
            .limits
            .max_bytes
            .checked_sub(self.usage.canonical_bytes)
            .ok_or(SnapshotBoundsError::CanonicalByteLimit)?;
        self.usage.canonical_bytes += bounded::canonical_graph_bytes(&graph, left)?;
        Ok(graph)
    }
    /// Exact node lookup; a missing row still consumes one visit.
    pub fn node(&mut self, id: &str) -> Result<Option<Node>, GraphError> {
        self.keys(&[id])?;
        Ok(self
            .read(
                Kind::Node,
                "WHERE id COLLATE BINARY=?1 LIMIT 1",
                &[id.to_owned().into()],
                1,
            )?
            .0
            .into_iter()
            .next())
    }
    fn page_rows(&mut self, n: usize) -> Result<usize, GraphError> {
        if n == 0 {
            return Err(SnapshotBoundsError::InvalidLimits.into());
        }
        if self.usage.adjacency_probes >= self.limits.max_rows
            || self.usage.visits >= self.limits.max_rows
        {
            return Err(SnapshotBoundsError::RowLimit.into());
        }
        self.usage.adjacency_probes += 1;
        Ok(n.min(self.limits.max_rows - self.usage.visits))
    }
    /// Read a BINARY primary-key range for one source, without label filters.
    pub fn outgoing(
        &mut self,
        root: &str,
        after: Option<&EdgeKey>,
        requested: usize,
    ) -> Result<AdjacencyPage, GraphError> {
        self.keys(&[root])?;
        if let Some(k) = after {
            self.keys(&[&k.src, &k.dst, &k.label])?;
            if k.src != root {
                return Err(SnapshotBoundsError::InvalidLimits.into());
            }
        }
        let n = self.page_rows(requested)?;
        let index = self.pk.replace('"', "\"\"");
        let (predicate, mut p) = if let Some(k) = after {
            (
                "src COLLATE BINARY=?1 AND (dst COLLATE BINARY,label COLLATE BINARY)>(?2,?3)",
                vec![
                    root.to_owned().into(),
                    k.dst.clone().into(),
                    k.label.clone().into(),
                ],
            )
        } else {
            ("src COLLATE BINARY=?1", vec![root.to_owned().into()])
        };
        p.push((n as i64).into());
        let suffix = format!(
            "INDEXED BY \"{index}\" WHERE {predicate} ORDER BY dst COLLATE BINARY,label COLLATE BINARY LIMIT ?{}",
            p.len()
        );
        let edges = self.read(Kind::Edge, &suffix, &p, n)?.1;
        let last = edges.last().map(|e| EdgeKey {
            src: e.src.clone(),
            dst: e.dst.clone(),
            label: e.label.clone(),
        });
        let more = if let Some(k) = &last {
            self.connection.query_row(&format!("SELECT EXISTS(SELECT 1 FROM edges INDEXED BY \"{index}\" WHERE src COLLATE BINARY=?1 AND (dst COLLATE BINARY,label COLLATE BINARY)>(?2,?3) LIMIT 1)"),[root,k.dst.as_str(),k.label.as_str()],|r|r.get::<_,bool>(0))?
        } else {
            false
        };
        Ok(AdjacencyPage {
            edges,
            remaining: if more {
                Remaining::More
            } else {
                Remaining::Exhausted
            },
            next_outgoing: if more { last } else { None },
            next_incoming: None,
        })
    }
    /// Read one destination index range in signed rowid order.
    pub fn incoming(
        &mut self,
        root: &str,
        after: Option<i64>,
        requested: usize,
    ) -> Result<AdjacencyPage, GraphError> {
        self.keys(&[root])?;
        let n = self.page_rows(requested)?;
        let (predicate, mut p) = if let Some(id) = after {
            (
                "dst COLLATE BINARY=?1 AND rowid>?2",
                vec![root.to_owned().into(), id.into()],
            )
        } else {
            ("dst COLLATE BINARY=?1", vec![root.to_owned().into()])
        };
        p.push((n as i64).into());
        let suffix = format!(
            "INDEXED BY idx_edges_dst WHERE {predicate} ORDER BY rowid LIMIT ?{}",
            p.len()
        );
        let edges = self.read(Kind::Edge, &suffix, &p, n)?.1;
        let last = edges
            .last()
            .map(|e| {
                self.connection.query_row(
                    "SELECT rowid FROM edges WHERE src COLLATE BINARY=?1 AND dst COLLATE BINARY=?2 AND label COLLATE BINARY=?3",
                    [&e.src, &e.dst, &e.label],
                    |r| r.get::<_, i64>(0),
                )
            })
            .transpose()?;
        let more = if let Some(id) = last {
            self.connection.query_row("SELECT EXISTS(SELECT 1 FROM edges INDEXED BY idx_edges_dst WHERE dst COLLATE BINARY=?1 AND rowid>?2 LIMIT 1)",rusqlite::params![root,id],|r|r.get::<_,bool>(0))?
        } else {
            false
        };
        Ok(AdjacencyPage {
            edges,
            remaining: if more {
                Remaining::More
            } else {
                Remaining::Exhausted
            },
            next_outgoing: None,
            next_incoming: if more { last } else { None },
        })
    }
}
#[cfg(test)]
mod tests;
