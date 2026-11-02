//! Bounded host-owned traversal; no durable cursor or runtime activation.
use super::{InvestigationBasisError, InvestigationReadBasis, InvestigationScopeExtent};
use core_graph::scoped::{EdgeKey, EdgeWindowRequest, NodeWindowRequest, Remaining};
use core_graph::{Edge, GraphError, Node, SnapshotReadLimits, SqliteGraphStore};

#[derive(Debug)]
enum Position {
    Nodes(Option<String>),
    Edges(Option<EdgeKey>),
    Done,
    Failed,
}

/// One raw page. The host separately admits transport and retained state.
#[derive(Debug)]
pub struct InvestigationPage {
    /// Nodes in this page, in storage key order.
    pub nodes: Vec<Node>,
    /// Edges in this page, in storage key order.
    pub edges: Vec<Edge>,
    /// Both raw traversal phases ended; not evidence of semantic completeness.
    pub traversal_complete: bool,
}

/// Failures never silently skip a row or permit this cursor to resume.
#[derive(Debug, thiserror::Error)]
pub enum InvestigationPageError {
    /// Limits must narrow existing ceilings and admit a nonempty window.
    #[error("invalid investigation page limits")]
    Limits,
    /// Failed acquisition must be handled by the host, not retried in this cursor.
    #[error("investigation traversal already failed")]
    Failed,
    /// Host basis changed or was not paged All.
    #[error(transparent)]
    Basis(#[from] InvestigationBasisError),
    /// Revision, structure or budget refusal from storage.
    #[error(transparent)]
    Storage(#[from] GraphError),
}

/// Constant-sized traversal state apart from bounded key strings. No graph is
/// accumulated here. Outer admission must also bound attempts and retained pages.
#[derive(Debug)]
pub struct InvestigationAllTraversal {
    basis: InvestigationReadBasis,
    page: SnapshotReadLimits,
    remaining: SnapshotReadLimits,
    raw_remaining: usize,
    position: Position,
}
impl InvestigationAllTraversal {
    /// Freeze a host-observed All basis and existing, explicitly narrowed limits.
    pub fn new(
        basis: InvestigationReadBasis,
        page: SnapshotReadLimits,
        aggregate: SnapshotReadLimits,
    ) -> Result<Self, InvestigationPageError> {
        if basis.observation.extent != InvestigationScopeExtent::AllPaged {
            return Err(InvestigationBasisError::InvalidExtent.into());
        }
        for l in [page, aggregate] {
            if l.max_rows == 0
                || l.max_rows > SnapshotReadLimits::MAX_ROWS
                || l.max_bytes == 0
                || l.max_bytes > SnapshotReadLimits::MAX_BYTES
                || l.max_values == 0
                || l.max_values > SnapshotReadLimits::MAX_VALUES
                || l.max_depth == 0
                || l.max_depth > SnapshotReadLimits::MAX_DEPTH
            {
                return Err(InvestigationPageError::Limits);
            }
        }
        Ok(Self {
            basis,
            page,
            remaining: aggregate,
            raw_remaining: aggregate.max_bytes,
            position: Position::Nodes(None),
        })
    }

    /// Acquire one page at the frozen revision. Any error poisons this traversal,
    /// including a changed host basis; no model call or source read occurs here.
    pub fn next(
        &mut self,
        store: &SqliteGraphStore,
        expected_basis: &str,
    ) -> Result<Option<InvestigationPage>, InvestigationPageError> {
        let result = self.acquire(store, expected_basis);
        if result.is_err() {
            self.position = Position::Failed;
        }
        result
    }

    fn acquire(
        &mut self,
        store: &SqliteGraphStore,
        expected_basis: &str,
    ) -> Result<Option<InvestigationPage>, InvestigationPageError> {
        self.basis.require_current(expected_basis)?;
        match self.position {
            Position::Failed => return Err(InvestigationPageError::Failed),
            Position::Done => return Ok(None),
            _ => {}
        }
        let limits = SnapshotReadLimits {
            max_rows: self.page.max_rows.min(self.remaining.max_rows),
            max_bytes: self
                .page
                .max_bytes
                .min(self.remaining.max_bytes)
                .min(self.raw_remaining),
            max_values: self.page.max_values.min(self.remaining.max_values),
            max_depth: self.page.max_depth.min(self.remaining.max_depth),
        };
        let revision = Some(self.basis.revision().clone());
        let (nodes, edges, rows, raw, canonical, values, position) = match &self.position {
            Position::Nodes(after) => {
                let w = store.read_node_window(&NodeWindowRequest {
                    expected_revision: revision,
                    after_id: after.clone(),
                    limits,
                })?;
                let next = if w.remaining == Remaining::More {
                    Position::Nodes(w.next_key)
                } else {
                    Position::Edges(None)
                };
                (
                    w.rows,
                    vec![],
                    w.scanned_raw_rows,
                    w.raw_bytes,
                    w.canonical_bytes,
                    w.json_values,
                    next,
                )
            }
            Position::Edges(after) => {
                let w = store.read_edge_window(&EdgeWindowRequest {
                    expected_revision: revision,
                    after_key: after.clone(),
                    limits,
                })?;
                let next = if w.remaining == Remaining::More {
                    Position::Edges(w.next_key)
                } else {
                    Position::Done
                };
                (
                    vec![],
                    w.rows,
                    w.scanned_raw_rows,
                    w.raw_bytes,
                    w.canonical_bytes,
                    w.json_values,
                    next,
                )
            }
            _ => unreachable!(),
        };
        // Storage enforced these narrowed bounds before copying/decoding rows.
        self.remaining.max_rows -= rows;
        self.remaining.max_bytes -= canonical;
        self.remaining.max_values -= values;
        self.raw_remaining -= raw;
        self.position = position;
        Ok(Some(InvestigationPage {
            nodes,
            edges,
            traversal_complete: matches!(self.position, Position::Done),
        }))
    }
}

#[cfg(test)]
mod tests;
