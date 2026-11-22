//! Filtered fact pages in context identity order at the pinned revision
//! (#1602). A whole-system investigation pages through these instead of
//! holding the complete graph.
use super::*;

/// The most labels one filter may name.
pub const MAX_FILTER_LABELS: usize = 64;

/// Which stored facts a page selects. An empty `labels` admits every label.
#[derive(Debug, Clone, Copy)]
pub struct FactFilter<'a> {
    /// Whether nodes are selected.
    pub nodes: bool,
    /// Whether edges are selected.
    pub edges: bool,
    /// The admitted labels; empty admits all.
    pub labels: &'a BTreeSet<String>,
}

impl FactFilter<'_> {
    fn clause(&self, first: usize) -> (String, Vec<SqlValue>) {
        if self.labels.is_empty() {
            return (String::new(), Vec::new());
        }
        let marks = (0..self.labels.len())
            .map(|i| format!("?{}", first + i))
            .collect::<Vec<_>>()
            .join(",");
        (
            format!("WHERE label COLLATE BINARY IN ({marks})"),
            self.labels.iter().cloned().map(SqlValue::from).collect(),
        )
    }
}

/// Nodes in id order; edges in (src, label, dst) order, the order
/// `ContextSnapshot` gives its facts. The edge index walk needs no sort.
const NODE_ORDER: &str = "ORDER BY id COLLATE BINARY";
const EDGE_INDEX: &str = "INDEXED BY idx_edges_src_label_dst";
const EDGE_ORDER: &str = "ORDER BY src COLLATE BINARY, label COLLATE BINARY, dst COLLATE BINARY";

impl ScopedGraphReader<'_> {
    fn check(&self, filter: &FactFilter<'_>) -> Result<(), GraphError> {
        if filter.labels.len() > MAX_FILTER_LABELS {
            return Err(SnapshotBoundsError::InvalidLimits.into());
        }
        self.keys(&filter.labels.iter().map(String::as_str).collect::<Vec<_>>())
    }

    /// The number of selected nodes and edges, counted without copying rows.
    pub fn count_facts(&mut self, filter: FactFilter<'_>) -> Result<(usize, usize), GraphError> {
        self.check(&filter)?;
        let (clause, params) = filter.clause(1);
        let count = |table: &str, on: bool| -> Result<usize, GraphError> {
            if !on {
                return Ok(0);
            }
            let n: i64 = self.connection().query_row(
                &format!("SELECT count(*) FROM {table} {clause}"),
                params_from_iter(&params),
                |r| r.get(0),
            )?;
            usize::try_from(n).map_err(|_| SnapshotBoundsError::InvalidRow.into())
        };
        Ok((count("nodes", filter.nodes)?, count("edges", filter.edges)?))
    }

    /// At most `limit` selected facts from `offset` in context identity order:
    /// the selected nodes first, then the selected edges. `nodes` is the
    /// selected node count from [`Self::count_facts`] at this revision. Every
    /// row is charged to this reader's cumulative budgets.
    pub fn fact_page(
        &mut self,
        filter: FactFilter<'_>,
        nodes: usize,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<Node>, Vec<Edge>), GraphError> {
        self.check(&filter)?;
        let (clause, params) = filter.clause(3);
        let mut page = (Vec::new(), Vec::new());
        let page_of = |order: &str, take: usize, skip: usize| {
            let mut p: Vec<SqlValue> = vec![(take as i64).into(), (skip as i64).into()];
            p.extend(params.iter().cloned());
            (format!("{clause} {order} LIMIT ?1 OFFSET ?2"), p)
        };
        let node_skip = offset.min(nodes);
        let node_take = if filter.nodes {
            limit.min(nodes - node_skip)
        } else {
            0
        };
        if node_take > 0 {
            let (suffix, p) = page_of(NODE_ORDER, node_take, node_skip);
            page.0 = self.read(Kind::Node, &suffix, &p, node_take)?.0;
            if page.0.len() != node_take {
                // The count and the page disagree at one pinned revision.
                return Err(SnapshotBoundsError::InvalidRow.into());
            }
        }
        let edge_take = limit - page.0.len();
        if filter.edges && edge_take > 0 {
            let (suffix, p) = page_of(EDGE_ORDER, edge_take, offset.saturating_sub(nodes));
            page.1 = self
                .read(Kind::Edge, &format!("{EDGE_INDEX} {suffix}"), &p, edge_take)?
                .1;
        }
        Ok(page)
    }
}
