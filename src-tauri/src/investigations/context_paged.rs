//! The whole-system ("Recovered system") scope without the whole graph
//! (#1602 option A, AC-1058). Preparation streams only the complete-graph
//! identity; each query then reads one filtered page at the pinned revision,
//! so a task holds the facts it has seen, not the estate.
use super::*;
#[cfg(test)]
use context_hub::QueryResponse;
use context_hub::{ContextError, ContextFact, FactSelection, FactSource};
use core_graph::GraphError;
use core_graph::scoped::{FactFilter, ScopedGraphReader};

/// A paged query's refusal: the selection's own, or the store's.
#[derive(Debug)]
pub(super) enum PagedError {
    Context(ContextError),
    Host(HostError),
}

impl From<ContextError> for PagedError {
    fn from(error: ContextError) -> Self {
        Self::Context(error)
    }
}

impl From<HostError> for PagedError {
    fn from(error: HostError) -> Self {
        Self::Host(error)
    }
}

impl From<GraphError> for PagedError {
    fn from(error: GraphError) -> Self {
        Self::Host(scoped_graph_error(error))
    }
}

/// The store at one pinned revision as a query's fact source. `rows` keeps
/// the stored copies of the facts the last page returned.
struct StoreFacts<'r, 'a> {
    reader: &'r mut ScopedGraphReader<'a>,
    rows: ScopedGraph,
}

impl FactSource for StoreFacts<'_, '_> {
    type Error = PagedError;

    fn select(
        &mut self,
        selection: &FactSelection,
        offset: usize,
        limit: usize,
    ) -> Result<(usize, Vec<ContextFact>), PagedError> {
        let (nodes, edges) = match &selection.scope {
            QueryScope::All => {
                let filter = FactFilter {
                    nodes: selection.kind != Some(FactKind::Edge),
                    edges: selection.kind != Some(FactKind::Node),
                    labels: &selection.labels,
                };
                let (nodes, edges) = self.reader.count_facts(filter)?;
                let total = nodes.checked_add(edges).ok_or(HostError::LimitExceeded)?;
                if offset >= total {
                    self.rows = (Vec::new(), Vec::new());
                    return Ok((total, Vec::new()));
                }
                self.rows = self.reader.fact_page(filter, nodes, offset, limit)?;
                let facts = self
                    .rows
                    .0
                    .iter()
                    .cloned()
                    .map(ContextFact::from_node)
                    .chain(self.rows.1.iter().cloned().map(ContextFact::from_edge))
                    .collect();
                return Ok((total, facts));
            }
            QueryScope::Node { id } => (
                vec![
                    self.reader
                        .node(id)?
                        .ok_or_else(|| ContextError::UnknownNode(id.clone()))?,
                ],
                Vec::new(),
            ),
            QueryScope::Neighborhood { anchor, hops } => {
                self.reader
                    .neighborhood(anchor, *hops)?
                    .ok_or_else(|| ContextError::UnknownAnchor(anchor.clone()))?
            }
        };
        // A node or neighborhood is small and read whole; its facts are then
        // selected exactly as the in-memory snapshot selects them.
        let scope = ContextSnapshot::new(nodes.clone(), edges.clone())?;
        let (total, facts) = (&scope).select(
            &FactSelection {
                scope: QueryScope::All,
                kind: selection.kind,
                labels: selection.labels.clone(),
            },
            offset,
            limit,
        )?;
        let returned: BTreeSet<_> = facts.iter().map(|f| f.reference.clone()).collect();
        self.rows = (
            nodes
                .into_iter()
                .filter(|n| returned.contains(&FactReference::Node { id: n.id.clone() }))
                .collect(),
            edges
                .into_iter()
                .filter(|e| {
                    returned.contains(&FactReference::Edge {
                        source: e.src.clone(),
                        label: e.label.clone(),
                        destination: e.dst.clone(),
                    })
                })
                .collect(),
        );
        Ok((total, facts))
    }
}

impl FrozenContext {
    /// Whole-system preparation: the complete-graph identity, streamed at a
    /// pinned revision, and no retained facts.
    pub(super) fn prepare_paged(
        state: &AppState,
        request: &AcquisitionRequest,
        history: &[String],
        graph_limits: SnapshotReadLimits,
    ) -> Result<Self, HostError> {
        let revision = state
            .graph
            .lock()
            .map_err(|_| HostError::Operational)?
            .read_revision()
            .map_err(scoped_graph_error)?;
        Self::prepare_paged_at(state, request, history, graph_limits, revision)
    }

    /// [`Self::prepare_paged`] at a revision the caller already read (with
    /// coverage, for a qualified task, AC-1060). A store that moved past it
    /// refuses with [`HostError::InputChanged`].
    pub(super) fn prepare_paged_at(
        state: &AppState,
        request: &AcquisitionRequest,
        history: &[String],
        graph_limits: SnapshotReadLimits,
        revision: StoreRevision,
    ) -> Result<Self, HostError> {
        let graph_id = complete_id_at(state, &revision, graph_limits)?;
        if request
            .expected_graph_revision
            .as_ref()
            .is_some_and(|expected| expected != &graph_id)
        {
            return Err(HostError::InputChanged);
        }
        let mut prepared = Self::assemble(
            (Vec::new(), Vec::new()),
            graph_id.clone(),
            ScopeFacts::Paged { id: graph_id },
            request.clone(),
            history,
        )?;
        prepared.graph_limits = graph_limits;
        prepared.scoped_revision = Some(revision);
        Ok(prepared)
    }
}

impl FrozenContext {
    /// The revision a paged read runs at: the pinned one, or, after a store
    /// write that left the complete graph identical (a replaced coverage
    /// inventory, say), the current one, found by streaming the identity
    /// again. The task keeps its frozen qualification either way (AC-0239);
    /// a changed graph is [`HostError::InputChanged`].
    pub(super) fn paged_revision(
        &self,
        state: &AppState,
        graph_id: &str,
    ) -> Result<StoreRevision, HostError> {
        let mut moved = self
            .moved_revision
            .lock()
            .map_err(|_| HostError::Operational)?;
        let pinned = moved
            .clone()
            .or_else(|| self.scoped_revision.clone())
            .ok_or(HostError::Operational)?;
        let current = state
            .graph
            .lock()
            .map_err(|_| HostError::Operational)?
            .read_revision()
            .map_err(scoped_graph_error)?;
        if current == pinned {
            return Ok(current);
        }
        if complete_id_at(state, &current, self.graph_limits)? != graph_id {
            return Err(HostError::InputChanged);
        }
        *moved = Some(current.clone());
        Ok(current)
    }
}

/// The complete-graph identity at `revision`, streamed one row at a time.
fn complete_id_at(
    state: &AppState,
    revision: &StoreRevision,
    graph_limits: SnapshotReadLimits,
) -> Result<String, HostError> {
    state
        .graph
        .lock()
        .map_err(|_| HostError::Operational)?
        .with_scoped_reader(revision, graph_limits, |reader| {
            Ok::<_, GraphError>(
                ContextSnapshot::complete_id_in(
                    reader,
                    IdentityPassLimits::for_retained(graph_limits),
                )
                .map_err(|error| match error {
                    context_hub::CompleteIdentityError::Graph(error) => scoped_graph_error(error),
                    context_hub::CompleteIdentityError::Context(error) => context_error(error),
                }),
            )
        })
        .map_err(scoped_graph_error)?
}

/// One query page of the complete graph `snapshot_id` at `revision`, and the
/// stored copies of the facts it returned.
#[cfg(test)]
pub(super) fn query_at(
    state: &AppState,
    revision: &StoreRevision,
    limits: SnapshotReadLimits,
    snapshot_id: &str,
    request: QueryRequest,
) -> Result<(QueryResponse, ScopedGraph), PagedError> {
    match page_at(state, revision, limits, snapshot_id, None, request)? {
        (InvestigationPageContext::Legacy(page), rows) => Ok((page, rows)),
        (InvestigationPageContext::Qualified(_), _) => {
            Err(PagedError::Host(HostError::Operational))
        }
    }
}

/// [`query_at`] for a task's page: under `qualified`, its coverage
/// qualification, when the task has one (AC-1060).
pub(super) fn page_at(
    state: &AppState,
    revision: &StoreRevision,
    limits: SnapshotReadLimits,
    snapshot_id: &str,
    qualified: Option<&CoverageContext>,
    request: QueryRequest,
) -> Result<(InvestigationPageContext, ScopedGraph), PagedError> {
    state
        .graph
        .lock()
        .map_err(|_| PagedError::Host(HostError::Operational))?
        .with_scoped_reader(revision, limits, |reader| {
            let mut source = StoreFacts {
                reader,
                rows: (Vec::new(), Vec::new()),
            };
            let response = match qualified {
                Some(qualified) => InvestigationPageContext::Qualified(qualified.query_source(
                    QualifiedRequest {
                        request,
                        context_id: Some(qualified.context_id().to_owned()),
                    },
                    &mut source,
                )?),
                None => InvestigationPageContext::Legacy(context_hub::query_source(
                    snapshot_id,
                    request,
                    &mut source,
                )?),
            };
            // The byte budget may return fewer facts than were read; keep
            // only the stored copies of those returned.
            let returned: BTreeSet<_> =
                response.page().facts.iter().map(|f| &f.reference).collect();
            let (nodes, edges) = source.rows;
            let rows = (
                nodes
                    .into_iter()
                    .filter(|n| returned.contains(&FactReference::Node { id: n.id.clone() }))
                    .collect(),
                edges
                    .into_iter()
                    .filter(|e| {
                        returned.contains(&FactReference::Edge {
                            source: e.src.clone(),
                            label: e.label.clone(),
                            destination: e.dst.clone(),
                        })
                    })
                    .collect(),
            );
            Ok((response, rows))
        })
}

#[cfg(test)]
impl FrozenContext {
    /// A qualified page of this context, held or paged, without the host's
    /// evidence inventory.
    pub(super) fn qualified_query(
        &self,
        state: &AppState,
        request: QualifiedRequest<QueryRequest>,
    ) -> Result<QualifiedResponse<QueryResponse>, PagedError> {
        let qualified = self
            .qualified
            .as_ref()
            .ok_or(PagedError::Host(HostError::InvalidInput))?;
        let ScopeFacts::Paged { id } = &self.snapshot else {
            return Ok(qualified.query(request)?);
        };
        let revision = self.paged_revision(state, id)?;
        state
            .graph
            .lock()
            .map_err(|_| PagedError::Host(HostError::Operational))?
            .with_scoped_reader(&revision, self.graph_limits, |reader| {
                let mut source = StoreFacts {
                    reader,
                    rows: (Vec::new(), Vec::new()),
                };
                qualified.query_source(request, &mut source)
            })
    }
}
