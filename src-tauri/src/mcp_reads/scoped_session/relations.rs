//! Scoped neighborhood relations (AC-1810): the induced neighborhood of one
//! visible node, acquired through the bounded revision-pinned reader and then
//! re-selected over the session's visible projection, so a hidden node never
//! bridges to a visible one. Exhausted acquisition budgets fail; a page is
//! never a truncated neighborhood or a complete-graph identity.
use super::*;
use context_hub::{
    ContextSnapshot, FactKind, QueryCursor, QueryRequest, QueryResponse, QueryScope,
};

#[derive(Debug)]
pub(crate) struct ScopedRelationsRequest {
    pub anchor: String,
    pub hops: usize,
    pub kind: Option<FactKind>,
    pub labels: Vec<String>,
    pub max_facts: usize,
    pub max_bytes: usize,
    pub cursor: Option<QueryCursor>,
}
#[derive(Debug, Serialize)]
pub(crate) struct ScopedRelationsPage {
    pub session_basis_id: String,
    pub revision: StoreRevision,
    pub coverage: CoverageQualification,
    pub complete_system: bool,
    pub absence_claim: bool,
    /// `snapshot_id` identifies this visible neighborhood's content only.
    pub page: QueryResponse,
}
impl ScopedReadSession {
    pub(crate) fn relations(
        &self,
        ctx: &mut ScopedReadContext<'_>,
        expected: &str,
        request: ScopedRelationsRequest,
    ) -> Result<ScopedRelationsPage, ReadError> {
        self.require_basis(expected)?;
        if request.hops == 0 || request.hops > context_hub::MAX_NEIGHBORHOOD_HOPS {
            return Err(ReadError::InvalidRequest);
        }
        self.check_current(ctx)?;
        let raw = graph(
            ctx.graph
                .lock()
                .map_err(|_| ReadError::Storage)?
                .with_scoped_reader(&self.revision, self.graph_limits, |reader| {
                    reader.neighborhood(&request.anchor, request.hops)
                }),
        )?;
        // A missing anchor and a hidden one are the same refusal.
        let (nodes, edges) = raw.ok_or(ReadError::InvalidRequest)?;
        let (visible_nodes, visible_edges) =
            spec::project_recovered_facts(&nodes, &edges, self.mode, &self.rejected);
        if !visible_nodes.iter().any(|node| node.id == request.anchor) {
            return Err(ReadError::InvalidRequest);
        }
        let snapshot = ContextSnapshot::new(
            visible_nodes.into_iter().cloned().collect(),
            visible_edges.into_iter().cloned().collect(),
        )
        .map_err(context_error)?;
        drop((nodes, edges));
        let envelope = ScopedRelationsPage {
            session_basis_id: self.basis_id.clone(),
            revision: self.revision.clone(),
            coverage: self.coverage.clone(),
            complete_system: false,
            absence_claim: false,
            page: QueryResponse {
                snapshot_id: snapshot.id().to_owned(),
                view: context_hub::ContextView::RecoveredGraph,
                total_selected: 0,
                facts: Vec::new(),
                next_cursor: None,
            },
        };
        // The core page budget covers its own response; charge the scoped
        // envelope around it. Facts are never skipped to fit later ones.
        let empty_page = serde_json::to_vec(&envelope.page)
            .map_err(|_| ReadError::Storage)?
            .len();
        let overhead = serde_json::to_vec(&envelope)
            .map_err(|_| ReadError::Storage)?
            .len()
            .checked_sub(empty_page)
            .ok_or(ReadError::Storage)?;
        let page_bytes = request
            .max_bytes
            .checked_sub(overhead)
            .filter(|n| *n > 0)
            .ok_or(ReadError::Limit)?;
        let page = snapshot
            .query(QueryRequest {
                scope: QueryScope::Neighborhood {
                    anchor: request.anchor,
                    hops: request.hops,
                },
                kind: request.kind,
                labels: request.labels,
                max_facts: request.max_facts,
                max_bytes: page_bytes.min(context_hub::MAX_RESPONSE_BYTES),
                cursor: request.cursor,
            })
            .map_err(context_error)?;
        self.check_current(ctx)?;
        let response = ScopedRelationsPage { page, ..envelope };
        admission::fits(&response, request.max_bytes)?;
        Ok(response)
    }
}
