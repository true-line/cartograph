#[cfg(unix)]
#[path = "context_preparation_workspace.rs"]
pub(crate) mod preparation_workspace;
use super::{AcquisitionSink, HostError, evidence::EvidenceGuards};
use crate::AppState;
use agents::investigation::*;
use agents::{AgentEvidence, TaskFactKey, TaskFactSelection, TaskRangeRole, TaskSourceAssociation};
use context_hub::coverage::{
    CoverageContext, CoverageQualification, CoverageStatus, QualifiedRequest, QualifiedResponse,
    ScopedRepo,
};
use context_hub::{
    ContextSnapshot, FactKind, FactReference, QueryCursor, QueryRequest, QueryScope,
};
use core_graph::coverage::CoverageReadLimits;
use core_graph::source::{FactKey, FactSourceSelection, FactSourceState};
use core_graph::{Edge, IdentityPassLimits, Node, SnapshotReadLimits, StoreRevision};
use core_prov::content_hash;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[path = "context_metadata_admission.rs"]
pub(super) mod metadata_admission;

#[path = "context_paged.rs"]
mod paged;

/// The facts a task's scope reads from (#1602).
pub(super) enum ScopeFacts {
    /// The scope's projection, held in memory.
    Held(Arc<ContextSnapshot>),
    /// The whole system, read a page at a time from the store at the pinned
    /// revision; `id` is its complete-graph identity (AC-1058).
    Paged { id: String },
}

impl ScopeFacts {
    pub(super) fn id(&self) -> &str {
        match self {
            Self::Held(snapshot) => snapshot.id(),
            Self::Paged { id } => id,
        }
    }

    /// The held projection; a paged scope holds none.
    pub(super) fn held(&self) -> Option<&Arc<ContextSnapshot>> {
        match self {
            Self::Held(snapshot) => Some(snapshot),
            Self::Paged { .. } => None,
        }
    }
}

/// One frozen fact's stored copy.
pub(super) enum FrozenFact<'a> {
    Node(&'a Node),
    Edge(&'a Edge),
}

/// No fresh rows: every fact is already in the context's graph.
pub(super) const NO_ROWS: &ScopedGraph = &(Vec::new(), Vec::new());

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "context_metadata_admission_tests.rs"]
mod metadata_admission_tests;

/// Validated acquisition authority, separate from every public request family.
/// Completion requests retain their actual role and selected immutable policy.
#[derive(Clone)]
pub(super) struct AcquisitionRequest {
    pub specialist_id: SpecialistId,
    pub inspection_requirements: Option<Vec<InvestigationInspectionTarget>>,
    pub question: String,
    pub scope: InvestigationScope,
    pub provider_mode: InvestigationProviderMode,
    pub expected_graph_revision: Option<String>,
    pub expected_context_id: Option<String>,
    pub conversation_id: Option<String>,
    pub parent_id: Option<String>,
    pub policy: Arc<InvestigationPolicy>,
}
impl TryFrom<&StartInvestigationRequest> for AcquisitionRequest {
    type Error = HostError;
    fn try_from(request: &StartInvestigationRequest) -> Result<Self, HostError> {
        request.validate()?;
        Ok(Self {
            specialist_id: request.specialist_id,
            inspection_requirements: request.inspection_requirements.clone(),
            question: request.question.clone(),
            scope: request.scope.clone(),
            provider_mode: request.provider_mode,
            expected_graph_revision: request.expected_graph_revision.clone(),
            expected_context_id: request.expected_context_id.clone(),
            conversation_id: request.conversation_id.clone(),
            parent_id: request.parent_id.clone(),
            policy: Arc::new(
                InvestigationPolicy::for_profile(&request.limit_profile)
                    .ok_or(HostError::InvalidInput)?
                    .clone(),
            ),
        })
    }
}
impl TryFrom<&AcquisitionRequest> for AcquisitionRequest {
    type Error = HostError;
    fn try_from(request: &AcquisitionRequest) -> Result<Self, HostError> {
        Ok(request.clone())
    }
}

/// `evidence` without the withheld reads, borrowed when none are withheld.
pub(super) fn supplied<'a>(
    evidence: &'a [AgentEvidence],
    withheld: &std::collections::BTreeSet<String>,
) -> std::borrow::Cow<'a, [AgentEvidence]> {
    if withheld.is_empty() {
        return std::borrow::Cow::Borrowed(evidence);
    }
    std::borrow::Cow::Owned(
        evidence
            .iter()
            .filter(|evidence| !withheld.contains(&evidence.id))
            .cloned()
            .collect(),
    )
}

/// One owner holds the original raw graph for exact comparisons and the frozen
/// authorized query projection. Nothing is rebuilt from live context mid-task.
pub(super) struct FrozenContext {
    pub(super) graph: (Vec<Node>, Vec<Edge>),
    pub(super) snapshot: ScopeFacts,
    pub(super) ledger: InvestigationInputLedger,
    pub(super) query_pages: Vec<String>,
    pub(super) evidence: Vec<AgentEvidence>,
    /// Acquired reads the firewall refused toward a cloud provider (#679).
    /// Their evidence stays here for ordinals and limits; prompts withhold it.
    pub(super) withheld: std::collections::BTreeSet<String>,
    pub(super) coverage_archive: Option<InvestigationCoverageArchive>,
    pub(super) inventory_details: Vec<String>,
    qualified: Option<CoverageContext>,
    pub(super) policy: Arc<InvestigationPolicy>,
    specialist: SpecialistId,
    /// The complete-graph ceiling this context was prepared under; later
    /// evidence selection rereads under the same bound (#1602).
    graph_limits: SnapshotReadLimits,
    /// The store revision a scope-aware or paged preparation pinned (#1602,
    /// AC-1302, AC-1058). `graph` then holds only the task's scope (for a
    /// paged scope, the facts its pages returned), and later
    /// selection rereads exactly the selected facts at this revision instead
    /// of the whole graph. `None` for a complete-graph preparation.
    scoped_revision: Option<StoreRevision>,
}

/// Owned local preparation, never a deserialized or cloneable publication token.
/// Query preparation may acquire retention locks. This binds the frozen basis;
/// it does not attest durable task ownership, cancellation or transaction state.
pub(super) struct PreparedContextRead {
    base_ledger: InvestigationInputLedger,
    policy_profile: &'static str,
    ledger: InvestigationInputLedger,
    raw: String,
    kind: PreparedContextReadKind,
    guards: Option<EvidenceGuards>,
    /// Stored copies of a paged page's facts, retained on publication.
    rows: ScopedGraph,
}

/// Inventory preparation plus the actual host publication wrapper. The frozen
/// context and accumulated prior reads remain caller owners.
#[cfg(unix)]
pub(crate) fn inventory_admission_workspace(replay: usize) -> Option<usize> {
    let model = inventory_preparation_workspace(&INVESTIGATION_V2_POLICY, MAX_MANIFEST_BYTES)?;
    let wrapper = std::mem::size_of::<PreparedContextRead>();
    let retained = model.retained.checked_add(wrapper)?;
    let peak = model.peak.checked_add(wrapper)?;
    Some(peak.max(retained.checked_add(replay)?))
}

/// Borrowed complete prospective context. The preparation and its retention
/// guards must outlive this view; no raw response or source text is cloned.
/// This is sizing input, not a durable publication or execution capability.
pub(super) struct ProspectiveContextView<'a> {
    pub ledger: &'a InvestigationInputLedger,
    pub query_pages: Vec<&'a str>,
    pub evidence: &'a [AgentEvidence],
    pub inventory_details: Vec<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreparedContextReadKind {
    Query,
    Inventory,
}

/// Next-family metadata preparation can identify the cumulative read-byte
/// budget; other host limits retain their ordinary HostError classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContextReadPreparationError {
    Host(HostError),
    QueryRejected(QueryRejection),
    QueryBudget(agents::investigation::query_refusal::QueryBudgetDiagnostic),
    CumulativeQueryAndInventoryBytes,
}
pub(super) use agents::investigation::query_refusal::QueryRejection;

/// Only errors from querying a validated frozen context can become feedback.
/// Construction, coverage and serialization errors must remain host failures.
fn query_preparation_error(error: context_hub::ContextError) -> ContextReadPreparationError {
    use context_hub::ContextError as E;
    let reason = match error {
        E::UnknownAnchor(_) => QueryRejection::UnknownAnchor,
        E::UnknownNode(_) => QueryRejection::UnknownNode,
        E::CursorSnapshotMismatch => QueryRejection::CursorSnapshotMismatch,
        E::CursorSelectionMismatch => QueryRejection::CursorSelectionMismatch,
        E::CursorContextMismatch => QueryRejection::CursorContextMismatch,
        E::InvalidCursorOffset { .. } => QueryRejection::InvalidCursorOffset,
        E::InvalidLimit { .. } => QueryRejection::InvalidLimit,
        other => return ContextReadPreparationError::Host(context_error(other)),
    };
    ContextReadPreparationError::QueryRejected(reason)
}
impl ContextReadPreparationError {
    pub(super) fn into_host_error(self) -> HostError {
        match self {
            Self::Host(error) => error,
            // Legacy callers remain terminal until a durable refusal is published.
            Self::QueryRejected(_) => HostError::InvalidInput,
            Self::CumulativeQueryAndInventoryBytes | Self::QueryBudget(_) => {
                HostError::LimitExceeded
            }
        }
    }
}
impl From<HostError> for ContextReadPreparationError {
    fn from(error: HostError) -> Self {
        Self::Host(error)
    }
}
impl From<InvestigationError> for ContextReadPreparationError {
    fn from(error: InvestigationError) -> Self {
        Self::Host(error.into())
    }
}

impl PreparedContextRead {
    pub(super) fn base_ledger(&self) -> &InvestigationInputLedger {
        &self.base_ledger
    }
    pub(super) fn policy_profile(&self) -> &'static str {
        self.policy_profile
    }
    pub(super) fn ledger(&self) -> &InvestigationInputLedger {
        &self.ledger
    }
    pub(super) fn raw(&self) -> &str {
        &self.raw
    }
    pub(super) fn kind(&self) -> PreparedContextReadKind {
        self.kind
    }
}

#[derive(Serialize)]
struct InvestigationQueryPage {
    context: InvestigationPageContext,
    evidence_options: Vec<EvidenceOptions>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum InvestigationPageContext {
    Legacy(context_hub::QueryResponse),
    Qualified(QualifiedResponse<context_hub::QueryResponse>),
}

impl InvestigationPageContext {
    fn page(&self) -> &context_hub::QueryResponse {
        match self {
            Self::Legacy(page) => page,
            Self::Qualified(page) => &page.response,
        }
    }
}

#[derive(Serialize)]
pub(super) struct EvidenceOptions {
    pub fact: TaskFactKey,
    pub origin: &'static str,
    pub ranges: Vec<EvidenceOption>,
}

#[derive(Serialize)]
pub(super) struct EvidenceOption {
    pub role: TaskRangeRole,
    pub index: u32,
    pub source: core_prov::EvidenceRef,
}

// Fixed JSON keys/punctuation outside proof and inventory entries. Proof must
// independently fit the existing manifest cap. Raw stored fields do not include
// JSON escaping or per-entry keys: their encoded expansion is checked below.
const ARCHIVE_ENVELOPE_BYTES: usize =
    b"{\"schema_version\":1,\"proof\":null,\"inventories\":[]}".len() - b"null".len();

pub(super) fn coverage_preparation_limits(
    archive_capacity: usize,
) -> Result<CoverageReadLimits, HostError> {
    let max_bytes = archive_capacity
        .min(MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES)
        .checked_sub(MAX_MANIFEST_BYTES + ARCHIVE_ENVELOPE_BYTES)
        .filter(|bytes| *bytes != 0)
        .ok_or(HostError::LimitExceeded)?;
    Ok(CoverageReadLimits {
        max_records: CoverageReadLimits::MAX_RECORDS,
        max_bytes: max_bytes.min(CoverageReadLimits::MAX_BYTES),
    })
}

/// Match the immutable archive wire without constructing an owned serialized
/// body. This runs before Archive::new's larger format-level validation buffer.
fn admit_archive_size(
    proof: &context_hub::coverage::CoverageProof,
    inventories: &[core_graph::coverage::StoredCoverage],
    maximum: usize,
) -> Result<(), HostError> {
    #[derive(Serialize)]
    struct Archive<'a> {
        schema_version: u32,
        proof: &'a context_hub::coverage::CoverageProof,
        inventories: &'a [core_graph::coverage::StoredCoverage],
    }
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("investigation archive byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(
        Count(maximum.min(MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES)),
        &Archive {
            schema_version: 1,
            proof,
            inventories,
        },
    )
    .map_err(|_| HostError::LimitExceeded)
}

// Counts actual application projection construction work in native tests only;
// this is structural ownership/work evidence, not allocator or RSS measurement.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ProjectionWork {
    constructions: usize,
    nodes: usize,
    edges: usize,
}
#[cfg(test)]
thread_local! {
    static PROJECTION_WORK: std::cell::Cell<ProjectionWork> = const { std::cell::Cell::new(ProjectionWork { constructions: 0, nodes: 0, edges: 0 }) };
}
#[cfg(test)]
fn take_projection_work() -> ProjectionWork {
    PROJECTION_WORK.with(std::cell::Cell::take)
}
/// The nodes and edges of one investigation scope.
pub(super) type ScopedGraph = (Vec<Node>, Vec<Edge>);

fn projection(nodes: Vec<Node>, edges: Vec<Edge>) -> Result<ContextSnapshot, HostError> {
    #[cfg(test)]
    PROJECTION_WORK.with(|counter| {
        let mut work = counter.get();
        work.constructions += 1;
        work.nodes += nodes.len();
        work.edges += edges.len();
        counter.set(work);
    });
    ContextSnapshot::new(nodes, edges).map_err(|_| HostError::InvalidInput)
}

/// The complete-graph ceiling for one investigation task (#1602). A task
/// started through MCP runs under MCP scope limits and keeps the interactive
/// default that its reservation was computed from; a desktop-started task may
/// read a whole estate up to the investigation ceiling.
pub(crate) fn task_graph_limits(mcp_started: bool) -> SnapshotReadLimits {
    if mcp_started {
        SnapshotReadLimits::default()
    } else {
        SnapshotReadLimits::investigation()
    }
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod graph_limit_tests {
    use super::*;

    // AC-0556 (#1602): MCP-started tasks keep the limits their worker
    // reservation was sized from; only desktop starts get the estate ceiling.
    #[test]
    fn mcp_started_tasks_keep_the_reserved_default_graph_limits() {
        let mcp = task_graph_limits(true);
        let desktop = task_graph_limits(false);
        assert_eq!(mcp, SnapshotReadLimits::default());
        assert_eq!(mcp.max_rows, 50_000);
        assert_eq!(mcp.max_bytes, 32 * 1024 * 1024);
        assert_eq!(desktop, SnapshotReadLimits::investigation());
        assert_eq!(desktop.max_rows, SnapshotReadLimits::MAX_ROWS);
        assert!(desktop.max_bytes > mcp.max_bytes);
    }
}

impl FrozenContext {
    /// The evidence a prompt may supply: every acquired read except those the
    /// firewall refused (#679). Borrowed when nothing is withheld.
    pub(super) fn supplied_evidence(&self) -> std::borrow::Cow<'_, [AgentEvidence]> {
        supplied(&self.evidence, &self.withheld)
    }
    #[cfg(test)]
    pub(super) fn completion_acquisition_request(
        request: &obligations::StagedObligationRequest,
    ) -> Result<AcquisitionRequest, HostError> {
        let policy = InvestigationPolicy::for_profile(&request.limit_profile)
            .ok_or(HostError::InvalidInput)?;
        Self::completion_acquisition_request_with_policy(request, policy)
    }

    pub(super) fn completion_acquisition_request_with_policy(
        request: &obligations::StagedObligationRequest,
        policy: &InvestigationPolicy,
    ) -> Result<AcquisitionRequest, HostError> {
        let request = request.normalized_with_policy(policy)?;
        let sources = request.obligations.sources_with_policy(policy)?;
        let specialist_id =
            serde_json::from_value(serde_json::Value::String(request.specialist_id.clone()))
                .map_err(|_| HostError::InvalidInput)?;
        Ok(AcquisitionRequest {
            specialist_id,
            inspection_requirements: if sources.is_empty() {
                None
            } else {
                Some(normalize_inspection_targets_with_policy(&sources, policy)?)
            },
            question: request.question,
            scope: request.scope,
            provider_mode: request.provider_mode,
            expected_graph_revision: request.expected_graph_revision,
            expected_context_id: Some(request.expected_context_id),
            conversation_id: request.conversation_id,
            parent_id: request.parent_id,
            policy: Arc::new(policy.clone()),
        })
    }

    /// A classified failure completes a real authorized read but never invents
    /// acquired bytes, citations, or a new acquisition revision.
    pub(super) fn read_evidence_completion(
        &mut self,
        state: &AppState,
        fact: TaskFactKey,
        role: TaskRangeRole,
        index: u32,
        sink: &mut impl AcquisitionSink,
    ) -> Result<Option<staged_attempt::UnavailableReason>, HostError> {
        if index >= 1024 || self.evidence.len() >= self.policy.limits().evidence_items {
            return Err(HostError::LimitExceeded);
        }
        let selected = self
            .ledger
            .selected_facts
            .iter()
            .find(|selected| selected.fact == fact)
            .cloned()
            .ok_or(HostError::InvalidInput)?;
        // Legacy registered-checkout reads cannot classify I/O as a pinned
        // capture outcome. Keep their existing terminal error semantics.
        if selected.binding.is_none() {
            self.read_evidence(state, fact, role, index, sink)?;
            return Ok(None);
        }
        let keys = self
            .ledger
            .selected_facts
            .iter()
            .map(|selected| crate::task_evidence::graph_fact(&selected.fact))
            .collect::<Vec<_>>();
        let preliminary = self.select(state, &keys)?;
        // Union retention failures may concern unrelated facts; never attribute
        // them to the requested obligation.
        let guards = EvidenceGuards::acquire(state, &preliminary)?;
        let options = match guards.options(state, self, NO_ROWS, &selected) {
            Ok(options) => options,
            Err(HostError::SourceUnavailable) => {
                return Ok(Some(staged_attempt::UnavailableReason::PinnedTargetAbsent));
            }
            Err(error) => return Err(error),
        };
        if !options
            .ranges
            .iter()
            .any(|range| range.role == role && range.index == index)
        {
            return Ok(Some(staged_attempt::UnavailableReason::RangeOutOfBounds));
        }
        // The existing read rechecks the union and may encounter operational
        // failures; its coarse errors are not per-target absence authority.
        self.read_evidence(state, fact, role, index, sink)?;
        Ok(None)
    }

    pub(super) fn read_inventory_completion(
        &mut self,
        request: InvestigationCoverageRead,
        sink: &mut impl AcquisitionSink,
    ) -> Result<Option<staged_attempt::UnavailableReason>, HostError> {
        self.ledger.validate_with_policy(&self.policy)?;
        let archive = self
            .coverage_archive
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        let proof = self
            .ledger
            .coverage
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        if proof != archive.proof()
            || request.context_id != proof.context_id
            || !(1..=MAX_QUERY_BYTES).contains(&request.max_bytes)
            || !proof.qualification.repos.iter().any(|repo| {
                repo.repo == request.repo
                    && repo.status == CoverageStatus::Recorded
                    && repo.inventory_hash.as_deref() == Some(request.inventory_hash.as_str())
            })
        {
            return Err(HostError::InvalidInput);
        }
        match archive.read(&request) {
            Err(InvestigationError::LimitExceeded) => Ok(Some(
                staged_attempt::UnavailableReason::InventoryExceedsMaxBytes,
            )),
            Err(error) => Err(error.into()),
            Ok(_) => {
                self.read_inventory(request, sink)?;
                Ok(None)
            }
        }
    }

    /// Desktop-ceiling preparation, kept for fixtures and tests.
    #[cfg(test)]
    pub(super) fn prepare(
        state: &AppState,
        request: impl TryInto<AcquisitionRequest, Error = HostError>,
        history: &[String],
    ) -> Result<Self, HostError> {
        Self::prepare_bounded(state, request, history, SnapshotReadLimits::investigation())
    }

    fn prepare_bounded(
        state: &AppState,
        request: impl TryInto<AcquisitionRequest, Error = HostError>,
        history: &[String],
        graph_limits: SnapshotReadLimits,
    ) -> Result<Self, HostError> {
        let request = request.try_into()?;
        if request.specialist_id.uses_coverage_qualification() {
            return Err(HostError::InvalidInput);
        }
        if let Some(prepared) = Self::prepare_scoped(state, &request, history, graph_limits)? {
            return Ok(prepared);
        }
        if request.scope == InvestigationScope::All {
            return Self::prepare_paged(state, &request, history, graph_limits);
        }
        let base = state
            .graph
            .lock()
            .map_err(|_| HostError::Operational)?
            .read_snapshot_with_revision_bounded(graph_limits)
            .map_err(graph_error)?;
        let area = super::area::files(state, &base.revision, &request.scope)?;
        let mut prepared = Self::from_graph_in(base.graph, &request, history, area.as_ref())?;
        prepared.graph_limits = graph_limits;
        Ok(prepared)
    }

    /// Scope-aware preparation (#1602 option A, AC-1301): a neighborhood or
    /// area task reads only its scope, through one pinned scoped reader, and
    /// streams the complete-graph `context-v1` identity inside the same
    /// transaction under the ADR-0052 identity-pass caps, so
    /// `graph_snapshot_id` is unchanged while memory tracks the scope. `None`
    /// for any other scope, which keeps the complete-graph read.
    fn prepare_scoped(
        state: &AppState,
        request: &AcquisitionRequest,
        history: &[String],
        graph_limits: SnapshotReadLimits,
    ) -> Result<Option<Self>, HostError> {
        if !scope_aware(&request.scope)? {
            return Ok(None);
        }
        let revision = state
            .graph
            .lock()
            .map_err(|_| HostError::Operational)?
            .read_revision()
            .map_err(scoped_graph_error)?;
        Self::prepare_scoped_at(state, request, history, graph_limits, revision).map(Some)
    }

    /// [`Self::prepare_scoped`] at a revision the caller already read (with
    /// coverage, for a qualified task). A store that moved past it refuses
    /// with [`HostError::InputChanged`].
    fn prepare_scoped_at(
        state: &AppState,
        request: &AcquisitionRequest,
        history: &[String],
        graph_limits: SnapshotReadLimits,
        revision: StoreRevision,
    ) -> Result<Self, HostError> {
        // The overlay is read without the graph lock, at this revision.
        let area = super::area::files(state, &revision, &request.scope)?;
        let (graph_id, graph) = Self::scoped_graph_at(
            state,
            &request.scope,
            area.as_ref(),
            graph_limits,
            &revision,
        )?;
        let mut prepared = Self::from_scoped(graph, graph_id, request, history)?;
        prepared.graph_limits = graph_limits;
        prepared.scoped_revision = Some(revision);
        Ok(prepared)
    }

    /// The complete-graph identity and only the scope's facts of a
    /// neighborhood or area `scope`, read through one scoped reader pinned at
    /// `revision` (AC-1301). `area` is the area's files at that revision.
    pub(super) fn scoped_graph_at(
        state: &AppState,
        scope: &InvestigationScope,
        area: Option<&BTreeSet<String>>,
        graph_limits: SnapshotReadLimits,
        revision: &StoreRevision,
    ) -> Result<(String, ScopedGraph), HostError> {
        state
            .graph
            .lock()
            .map_err(|_| HostError::Operational)?
            // The store's own errors pass through `E: From<GraphError>`;
            // a refusal decided inside the read rides out as the inner result.
            .with_scoped_reader(revision, graph_limits, |reader| {
                Ok::<_, core_graph::GraphError>((|| {
                    let graph_id = ContextSnapshot::complete_id_in(
                        reader,
                        IdentityPassLimits::for_retained(graph_limits),
                    )
                    .map_err(|error| match error {
                        context_hub::CompleteIdentityError::Graph(error) => {
                            scoped_graph_error(error)
                        }
                        context_hub::CompleteIdentityError::Context(error) => context_error(error),
                    })?;
                    let graph = match scope {
                        InvestigationScope::Neighborhood { anchor, hops } => reader
                            .neighborhood(anchor, *hops as usize)
                            .map_err(scoped_graph_error)?
                            .ok_or(HostError::InvalidInput)?,
                        _ => super::area::scoped_members(
                            reader,
                            area.ok_or(HostError::InvalidInput)?,
                        )
                        .map_err(scoped_graph_error)?,
                    };
                    Ok::<_, HostError>((graph_id, graph))
                })())
            })
            .map_err(scoped_graph_error)?
    }

    /// [`Self::scoped_graph_at`] as the complete-graph identity and the
    /// scope's projection, the same `scope_snapshot_id` a full read gives.
    pub(super) fn scoped_snapshot_at(
        state: &AppState,
        scope: &InvestigationScope,
        area: Option<&BTreeSet<String>>,
        graph_limits: SnapshotReadLimits,
        revision: &StoreRevision,
    ) -> Result<(String, ContextSnapshot), HostError> {
        let (graph_id, graph) = Self::scoped_graph_at(state, scope, area, graph_limits, revision)?;
        Ok((graph_id, projection(graph.0, graph.1)?))
    }

    /// A context whose graph is already the task's scope, with the
    /// complete-graph identity computed separately.
    fn from_scoped(
        graph: (Vec<Node>, Vec<Edge>),
        graph_id: String,
        request: &AcquisitionRequest,
        history: &[String],
    ) -> Result<Self, HostError> {
        if request
            .expected_graph_revision
            .as_ref()
            .is_some_and(|expected| expected != &graph_id)
        {
            return Err(HostError::InputChanged);
        }
        let snapshot = projection(graph.0.clone(), graph.1.clone())?;
        Self::assemble(
            graph,
            graph_id,
            ScopeFacts::Held(Arc::new(snapshot)),
            request.clone(),
            history,
        )
    }

    /// The app-backed path brackets registration/plugin discovery around one
    /// bounded graph+inventory read. Legacy preparation keeps its old path.
    pub(super) fn prepare_with_app<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        state: &AppState,
        request: impl TryInto<AcquisitionRequest, Error = HostError>,
        history: &[String],
        archive_capacity: Option<usize>,
    ) -> Result<Self, HostError> {
        Self::prepare_with_app_scope(app, state, request, history, archive_capacity, None)
    }

    pub(super) fn prepare_with_app_scope<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        state: &AppState,
        request: impl TryInto<AcquisitionRequest, Error = HostError>,
        history: &[String],
        archive_capacity: Option<usize>,
        scope_limits: Option<crate::mcp_reads::scope::ScopeLimits>,
    ) -> Result<Self, HostError> {
        let request = request.try_into()?;
        let graph_limits = task_graph_limits(scope_limits.is_some());
        if !request.specialist_id.uses_coverage_qualification() {
            return Self::prepare_bounded(state, &request, history, graph_limits);
        }
        let archive_capacity = archive_capacity.ok_or(HostError::InvalidInput)?;
        Self::prepare_qualified_bounded(
            state,
            &request,
            history,
            archive_capacity,
            graph_limits,
            || match scope_limits {
                Some(limits) => crate::mcp_reads::scope::coverage_scope(app, state, limits)
                    .map_err(|error| match error {
                        crate::mcp_reads::ReadError::Limit => HostError::LimitExceeded,
                        _ => HostError::Operational,
                    }),
                None => {
                    crate::context::coverage_scope(app, state).map_err(|_| HostError::Operational)
                }
            },
        )
    }

    #[cfg(test)]
    fn prepare_qualified_with_scope(
        state: &AppState,
        request: impl TryInto<AcquisitionRequest, Error = HostError>,
        history: &[String],
        archive_capacity: usize,
        scope: impl FnMut() -> Result<Vec<ScopedRepo>, HostError>,
    ) -> Result<Self, HostError> {
        Self::prepare_qualified_bounded(
            state,
            request,
            history,
            archive_capacity,
            SnapshotReadLimits::investigation(),
            scope,
        )
    }

    fn prepare_qualified_bounded(
        state: &AppState,
        request: impl TryInto<AcquisitionRequest, Error = HostError>,
        history: &[String],
        archive_capacity: usize,
        graph_limits: SnapshotReadLimits,
        mut scope: impl FnMut() -> Result<Vec<ScopedRepo>, HostError>,
    ) -> Result<Self, HostError> {
        let request = request.try_into()?;
        if !request.specialist_id.uses_coverage_qualification() {
            return Err(HostError::InvalidInput);
        }
        let coverage_limits = coverage_preparation_limits(archive_capacity)?;
        let before = scope()?;
        let (mut prepared, recorded) = if scope_aware(&request.scope)? {
            // Coverage and the scoped facts describe one revision: the
            // scoped reader refuses if the store moved after this read.
            let (revision, recorded) = state
                .graph
                .lock()
                .map_err(|_| HostError::Operational)?
                .read_coverage_records_with_revision_bounded(coverage_limits)
                .map_err(graph_error)?;
            let prepared =
                Self::prepare_scoped_at(state, &request, history, graph_limits, revision)?;
            (prepared, recorded)
        } else {
            let (base, recorded) = state
                .graph
                .lock()
                .map_err(|_| HostError::Operational)?
                .read_snapshot_with_coverage_and_revision_bounded(graph_limits, coverage_limits)
                .map_err(graph_error)?;
            let area = super::area::files(state, &base.revision, &request.scope)?;
            let prepared = Self::from_graph_in(base.graph, &request, history, area.as_ref())?;
            (prepared, recorded)
        };
        let stable = crate::context::stable_scope(&before, scope()?);
        let qualification =
            CoverageQualification::new(&stable, &recorded).map_err(context_error)?;
        prepared.graph_limits = graph_limits;
        // Both paths retain the one already-authorized immutable scoped
        // projection from this same bounded graph read.
        let qualified = CoverageContext::from_shared_snapshot(
            Arc::clone(prepared.snapshot.held().ok_or(HostError::Operational)?),
            qualification,
        )
        .map_err(context_error)?;
        let proof = qualified.durable_proof().map_err(context_error)?;
        bounded_json(&proof, MAX_MANIFEST_BYTES)?;
        if request
            .expected_context_id
            .as_ref()
            .is_some_and(|expected| expected != &proof.context_id)
        {
            return Err(HostError::InputChanged);
        }
        let recorded_repos: BTreeSet<_> = proof
            .qualification
            .repos
            .iter()
            .filter(|repo| repo.status == CoverageStatus::Recorded)
            .map(|repo| repo.repo.as_str())
            .collect();
        let originals: Vec<_> = recorded
            .into_iter()
            .filter(|stored| recorded_repos.contains(stored.record.repo_key.as_str()))
            .collect();
        admit_archive_size(&proof, &originals, archive_capacity)?;
        let archive = InvestigationCoverageArchive::new(proof.clone(), originals)?;
        prepared.ledger.schema_version = 2;
        prepared.ledger.coverage = Some(proof);
        prepared.ledger.validate_with_policy(&prepared.policy)?;
        prepared.coverage_archive = Some(archive);
        prepared.qualified = Some(qualified);
        Ok(prepared)
    }

    /// A graph with no area scope, for tests that freeze one directly.
    #[cfg(test)]
    fn from_graph(
        graph: (Vec<Node>, Vec<Edge>),
        request: impl TryInto<AcquisitionRequest, Error = HostError>,
        history: &[String],
    ) -> Result<Self, HostError> {
        Self::from_graph_in(graph, request, history, None)
    }

    /// [`Self::from_graph`] with the member files of the area the request is
    /// scoped to, when it is scoped to one.
    fn from_graph_in(
        graph: (Vec<Node>, Vec<Edge>),
        request: impl TryInto<AcquisitionRequest, Error = HostError>,
        history: &[String],
        area: Option<&BTreeSet<String>>,
    ) -> Result<Self, HostError> {
        let request = request.try_into()?;
        let complete = projection(graph.0.clone(), graph.1.clone())?;
        let graph_id = complete.id().to_owned();
        if request
            .expected_graph_revision
            .as_ref()
            .is_some_and(|expected| expected != &graph_id)
        {
            return Err(HostError::InputChanged);
        }
        let snapshot = ScopeFacts::Held(Arc::new(Self::scoped_snapshot(
            &graph,
            &request.scope,
            area,
            complete,
        )?));
        Self::assemble(graph, graph_id, snapshot, request, history)
    }

    fn assemble(
        graph: (Vec<Node>, Vec<Edge>),
        graph_id: String,
        snapshot: ScopeFacts,
        request: AcquisitionRequest,
        history: &[String],
    ) -> Result<Self, HostError> {
        let history_bytes = if history.is_empty() {
            Vec::new()
        } else {
            bounded_json(history, request.policy.supplied_history_bytes())?
        };
        let ledger = InvestigationInputLedger {
            schema_version: 1,
            coverage: None,
            coverage_reads: vec![],
            graph_snapshot_id: graph_id,
            scope_snapshot_id: snapshot.id().into(),
            revision: 0,
            selected_facts: vec![],
            receipt_references: vec![],
            citations: vec![],
            queries: vec![],
            supplied_history_hash: (!history_bytes.is_empty())
                .then(|| content_hash(&history_bytes)),
            supplied_history_bytes: history_bytes.len(),
        };
        ledger.validate_with_policy(&request.policy)?;
        Ok(Self {
            graph,
            snapshot,
            ledger,
            query_pages: vec![],
            evidence: vec![],
            withheld: Default::default(),
            coverage_archive: None,
            inventory_details: vec![],
            qualified: None,
            policy: Arc::clone(&request.policy),
            specialist: request.specialist_id,
            graph_limits: SnapshotReadLimits::investigation(),
            scoped_revision: None,
        })
    }

    pub(super) fn scoped_snapshot(
        graph: &(Vec<Node>, Vec<Edge>),
        requested: &InvestigationScope,
        area: Option<&BTreeSet<String>>,
        complete: ContextSnapshot,
    ) -> Result<ContextSnapshot, HostError> {
        Ok(match requested {
            InvestigationScope::Node { .. } => return Err(HostError::InvalidInput),
            InvestigationScope::All => complete,
            InvestigationScope::Area { .. } => {
                // The caller resolved the area against the overlay at this
                // graph's revision; without that there is no membership.
                let (nodes, edges) =
                    super::area::members(graph, area.ok_or(HostError::InvalidInput)?);
                projection(nodes, edges)?
            }
            InvestigationScope::Neighborhood { .. } => {
                // Reuse the service's actual scope selection rather than a
                // subtly different application-specific graph traversal.
                let references = complete
                    .references_in_scope(scope(requested))
                    .map_err(context_error)?;
                let nodes = graph
                    .0
                    .iter()
                    .filter(|n| references.contains(&FactReference::Node { id: n.id.clone() }))
                    .cloned()
                    .collect();
                let edges = graph
                    .1
                    .iter()
                    .filter(|e| {
                        references.contains(&FactReference::Edge {
                            source: e.src.clone(),
                            label: e.label.clone(),
                            destination: e.dst.clone(),
                        })
                    })
                    .cloned()
                    .collect();
                projection(nodes, edges)?
            }
        })
    }

    #[cfg(test)]
    pub(super) fn query(
        &mut self,
        state: &AppState,
        query: InvestigationQuery,
        sink: &mut impl AcquisitionSink,
    ) -> Result<(), HostError> {
        let prepared = self.prepare_query(state, query)?;
        self.publish_context_read(prepared, sink)
    }

    /// Build exact prospective bytes and metadata under retained source guards.
    /// This may touch lockfiles, but neither publishes input nor mutates self.
    #[cfg(test)]
    pub(super) fn prepare_query(
        &self,
        state: &AppState,
        query: InvestigationQuery,
    ) -> Result<PreparedContextRead, HostError> {
        self.prepare_query_with_diagnostic(state, query)
            .map_err(ContextReadPreparationError::into_host_error)
    }

    /// Next-family callers may retain the exact cumulative-read-budget cause.
    pub(super) fn prepare_query_with_diagnostic(
        &self,
        state: &AppState,
        query: InvestigationQuery,
    ) -> Result<PreparedContextRead, ContextReadPreparationError> {
        query.validate_for_specialist(self.specialist)?;
        self.ledger.validate_with_policy(&self.policy)?;
        match (
            &self.qualified,
            &self.ledger.coverage,
            &self.coverage_archive,
        ) {
            (None, None, None) => {}
            (Some(qualified), Some(proof), Some(archive))
                if qualified.context_id() == proof.context_id
                    && qualified.coverage_id() == proof.qualification.coverage_id
                    && qualified.snapshot_id() == self.snapshot.id()
                    && proof == archive.proof() => {}
            _ => return Err(HostError::InvalidInput.into()),
        }
        let query_page = |max_facts| {
            let request = QueryRequest {
                scope: scope(&query.scope),
                kind: query.kind.map(|k| match k {
                    InvestigationFactKind::Node => FactKind::Node,
                    InvestigationFactKind::Edge => FactKind::Edge,
                }),
                labels: query.labels.clone(),
                max_facts,
                max_bytes: query.max_bytes,
                cursor: query.cursor.as_ref().map(|c| QueryCursor {
                    snapshot_id: c.snapshot_id.clone(),
                    selection_id: c.selection_id.clone(),
                    offset: c.offset,
                }),
            };
            let read = match (&self.qualified, &self.snapshot, &self.scoped_revision) {
                (Some(qualified), _, _) => qualified
                    .query(QualifiedRequest {
                        request,
                        context_id: Some(qualified.context_id().to_owned()),
                    })
                    .map(|page| {
                        (
                            InvestigationPageContext::Qualified(page),
                            Default::default(),
                        )
                    })
                    .map_err(paged::PagedError::Context),
                (None, ScopeFacts::Held(snapshot), _) => snapshot
                    .query(request)
                    .map(|page| (InvestigationPageContext::Legacy(page), Default::default()))
                    .map_err(paged::PagedError::Context),
                (None, ScopeFacts::Paged { id }, Some(revision)) => {
                    paged::query_at(state, revision, self.graph_limits, id, request)
                        .map(|(page, rows)| (InvestigationPageContext::Legacy(page), rows))
                }
                (None, ScopeFacts::Paged { .. }, None) => {
                    Err(paged::PagedError::Host(HostError::Operational))
                }
            };
            read.map_err(|error| {
                let error = match error {
                    paged::PagedError::Context(error) => error,
                    paged::PagedError::Host(error) => return error.into(),
                };
                if self.specialist.uses_query_budget_refusals()
                    && let context_hub::ContextError::ResponseBudgetExceeded {
                        max_bytes,
                        required_bytes: _,
                    } = error
                {
                    use agents::investigation::query_refusal::{
                        QueryBudgetDiagnostic, QueryBudgetStage, RequiredBytesKind,
                    };
                    return ContextReadPreparationError::QueryBudget(QueryBudgetDiagnostic {
                        stage: if self.qualified.is_some() && max_bytes == query.max_bytes {
                            QueryBudgetStage::QualificationEnvelope
                        } else {
                            QueryBudgetStage::CorePage
                        },
                        requested_bytes: query.max_bytes,
                        effective_bytes: max_bytes,
                        // Core sizes precede egress privacy filtering. Never expose
                        // a fact's raw serialized size, even within frozen scope.
                        required_bytes: max_bytes.saturating_add(1),
                        required_bytes_kind: RequiredBytesKind::AtLeast,
                    });
                }
                query_preparation_error(error)
            })
        };
        let prior_keys = self
            .ledger
            .selected_facts
            .iter()
            .map(|s| crate::task_evidence::graph_fact(&s.fact))
            .collect::<BTreeSet<_>>();
        let (page, mut rows) = query_page(query.max_facts)?;
        let mut keys = prior_keys.clone();
        keys.extend(
            page.page()
                .facts
                .iter()
                .map(|f| graph_reference(&f.reference)),
        );
        if keys.len() > self.policy.limits().selected_facts {
            return Err(HostError::LimitExceeded.into());
        }
        let keys = keys.into_iter().collect::<Vec<_>>();
        let preliminary = self.select_with(state, &keys, &rows)?;
        let guards = EvidenceGuards::acquire(state, &preliminary)?;
        let authoritative = self.select_with(state, &keys, &rows)?;
        if authoritative != preliminary {
            return Err(HostError::InputChanged.into());
        }
        drop(preliminary);
        drop(keys);
        let mut options = Vec::new();
        for fact in &page.page().facts {
            let selected = authoritative
                .get(&graph_reference(&fact.reference))
                .ok_or(HostError::InvalidInput)?;
            options.push(guards.options(state, self, &rows, selected)?);
        }
        let mut response = InvestigationQueryPage {
            context: page,
            evidence_options: options,
        };
        let raw = loop {
            match bounded_json(&response, query.max_bytes) {
                Ok(raw) => break raw,
                Err(HostError::LimitExceeded) if response.context.page().facts.len() > 1 => {
                    // The core page budget excludes this host's evidence inventory.
                    // Shrink only the ordered suffix and let the frozen service
                    // produce the exact continuation; never skip a large fact.
                    let InvestigationQueryPage {
                        context,
                        mut evidence_options,
                    } = response;
                    let max_facts = context.page().facts.len() - 1;
                    // Release the old page before constructing its replacement.
                    drop(context);
                    let (context, fresh) = query_page(max_facts)?;
                    rows = fresh;
                    evidence_options.truncate(context.page().facts.len());
                    response = InvestigationQueryPage {
                        context,
                        evidence_options,
                    };
                }
                Err(HostError::LimitExceeded) if self.specialist.uses_query_budget_refusals() => {
                    use agents::investigation::query_refusal::{
                        QueryBudgetDiagnostic, QueryBudgetStage, RequiredBytesKind,
                    };
                    return Err(ContextReadPreparationError::QueryBudget(
                        QueryBudgetDiagnostic {
                            stage: QueryBudgetStage::EvidenceOptionsEnvelope,
                            requested_bytes: query.max_bytes,
                            effective_bytes: query.max_bytes,
                            required_bytes: query
                                .max_bytes
                                .checked_add(1)
                                .ok_or(HostError::LimitExceeded)?,
                            required_bytes_kind: RequiredBytesKind::AtLeast,
                        },
                    ));
                }
                Err(error) => return Err(error.into()),
            }
        };
        let mut returned_keys = prior_keys;
        returned_keys.extend(
            response
                .context
                .page()
                .facts
                .iter()
                .map(|f| graph_reference(&f.reference)),
        );
        let mut ledger = self.ledger.clone();
        ledger.revision = ledger
            .revision
            .checked_add(1)
            .ok_or(HostError::LimitExceeded)?;
        ledger.selected_facts = authoritative
            .iter()
            .filter(|(key, _)| returned_keys.contains(*key))
            .map(|(_, selected)| selected.clone())
            .collect();
        ledger.receipt_references = guards
            .references()
            .into_iter()
            .filter(|reference| {
                ledger
                    .selected_facts
                    .iter()
                    .filter_map(|s| s.binding.as_ref())
                    .any(|binding| {
                        binding.repo_key == reference.repo_key
                            && binding.receipt_id == reference.receipt_id
                    })
            })
            .collect();
        for fact in &response.context.page().facts {
            let key = graph_reference(&fact.reference);
            let selected = authoritative.get(&key).ok_or(HostError::InvalidInput)?;
            if !ledger.citations.iter().any(|c| {
                c.fact == selected.fact
                    && matches!(c.origin, InvestigationEvidenceOrigin::GraphMetadata)
            }) {
                let index = ledger
                    .citations
                    .iter()
                    .filter(|c| matches!(c.origin, InvestigationEvidenceOrigin::GraphMetadata))
                    .count()
                    + 1;
                ledger.citations.push(InvestigationCitation {
                    citation_id: format!("fact-{index}"),
                    fact: selected.fact.clone(),
                    fact_digest: selected.fact_digest.clone(),
                    source: None,
                    role: None,
                    index: None,
                    text_hash: None,
                    origin: InvestigationEvidenceOrigin::GraphMetadata,
                });
            }
        }
        InvestigationQueryBudget::from_ledger(&ledger)
            .and_then(|budget| budget.admit(raw.len()))
            .map_err(|_| ContextReadPreparationError::CumulativeQueryAndInventoryBytes)?;
        ledger.queries.push(InvestigationQueryManifest {
            query,
            response_hash: content_hash(&raw),
            response_bytes: raw.len(),
            returned_facts: response.context.page().facts.len(),
            total_selected: response.context.page().total_selected,
            has_more: response.context.page().next_cursor.is_some(),
        });
        drop(response);
        drop(authoritative);
        drop(returned_keys);
        ledger.validate_with_policy(&self.policy)?;
        let raw = String::from_utf8(raw).map_err(|_| HostError::InvalidInput)?;
        Ok(PreparedContextRead {
            base_ledger: self.ledger.clone(),
            policy_profile: self.policy.profile(),
            ledger,
            raw,
            kind: PreparedContextReadKind::Query,
            guards: Some(guards),
            rows,
        })
    }

    /// Acquire exact original inventory detail without consulting graph or
    /// source storage. Its response is untrusted coverage data, never a citation.
    pub(super) fn read_inventory(
        &mut self,
        request: InvestigationCoverageRead,
        sink: &mut impl AcquisitionSink,
    ) -> Result<(), HostError> {
        let prepared = self.prepare_inventory(request)?;
        self.publish_context_read(prepared, sink)
    }

    /// Prepare retained original inventory bytes without source or graph reads,
    /// publication, or mutation of the frozen acquisition state.
    pub(super) fn prepare_inventory(
        &self,
        request: InvestigationCoverageRead,
    ) -> Result<PreparedContextRead, HostError> {
        self.prepare_inventory_with_diagnostic(request)
            .map_err(ContextReadPreparationError::into_host_error)
    }

    /// Next-family callers may retain the exact cumulative-read-budget cause.
    pub(super) fn prepare_inventory_with_diagnostic(
        &self,
        request: InvestigationCoverageRead,
    ) -> Result<PreparedContextRead, ContextReadPreparationError> {
        self.ledger.validate_with_policy(&self.policy)?;
        let archive = self
            .coverage_archive
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        let proof = self
            .ledger
            .coverage
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        if proof != archive.proof()
            || self
                .qualified
                .as_ref()
                .is_none_or(|qualified| qualified.context_id() != proof.context_id)
        {
            return Err(HostError::InvalidInput.into());
        }
        let detail = archive.read(&request)?;
        let raw = bounded_json(&detail, request.max_bytes)?;
        InvestigationQueryBudget::from_ledger(&self.ledger)
            .and_then(|budget| budget.admit(raw.len()))
            .map_err(|_| ContextReadPreparationError::CumulativeQueryAndInventoryBytes)?;
        let mut ledger = self.ledger.clone();
        ledger.revision = ledger
            .revision
            .checked_add(1)
            .ok_or(HostError::LimitExceeded)?;
        ledger
            .coverage_reads
            .push(InvestigationCoverageReadManifest {
                request,
                response_hash: content_hash(&raw),
                response_bytes: raw.len(),
            });
        ledger.validate_with_policy(&self.policy)?;
        let raw = String::from_utf8(raw).map_err(|_| HostError::InvalidInput)?;
        Ok(PreparedContextRead {
            base_ledger: self.ledger.clone(),
            policy_profile: self.policy.profile(),
            ledger,
            raw,
            kind: PreparedContextReadKind::Inventory,
            guards: None,
            rows: Default::default(),
        })
    }

    /// Check the complete prospective representation against the frozen base.
    /// Publication repeats this check after any intervening work.
    fn validate_prepared_context(&self, prepared: &PreparedContextRead) -> Result<(), HostError> {
        if prepared.base_ledger() != &self.ledger
            || prepared.policy_profile() != self.policy.profile()
        {
            return Err(HostError::InputChanged);
        }
        // A candidate publishes exactly one acquisition of its declared kind.
        // Retain every prior manifest verbatim, including replay history.
        let base = prepared.base_ledger();
        let next = prepared.ledger();
        let one_manifest = match prepared.kind() {
            PreparedContextReadKind::Query => {
                base.queries.len().checked_add(1) == Some(next.queries.len())
                    && next.queries.starts_with(&base.queries)
                    && next.coverage_reads == base.coverage_reads
            }
            PreparedContextReadKind::Inventory => {
                base.coverage_reads.len().checked_add(1) == Some(next.coverage_reads.len())
                    && next.coverage_reads.starts_with(&base.coverage_reads)
                    && next.queries == base.queries
            }
        };
        if !one_manifest || base.revision.checked_add(1) != Some(next.revision) {
            return Err(HostError::InvalidInput);
        }
        prepared.ledger().validate_with_policy(&self.policy)?;
        let (response_bytes, response_hash) = match prepared.kind() {
            PreparedContextReadKind::Query => prepared
                .ledger()
                .queries
                .last()
                .map(|manifest| (manifest.response_bytes, &manifest.response_hash)),
            PreparedContextReadKind::Inventory => prepared
                .ledger()
                .coverage_reads
                .last()
                .map(|manifest| (manifest.response_bytes, &manifest.response_hash)),
        }
        .ok_or(HostError::InvalidInput)?;
        if response_bytes != prepared.raw().len()
            || response_hash != &content_hash(prepared.raw().as_bytes())
        {
            return Err(HostError::InvalidInput);
        }
        Ok(())
    }

    pub(super) fn prospective_context<'a>(
        &'a self,
        prepared: &'a PreparedContextRead,
    ) -> Result<ProspectiveContextView<'a>, HostError> {
        self.validate_prepared_context(prepared)?;
        let mut query_pages = Vec::new();
        query_pages
            .try_reserve(self.query_pages.len().saturating_add(1))
            .map_err(|_| HostError::LimitExceeded)?;
        query_pages.extend(self.query_pages.iter().map(String::as_str));
        let mut inventory_details = Vec::new();
        inventory_details
            .try_reserve(self.inventory_details.len().saturating_add(1))
            .map_err(|_| HostError::LimitExceeded)?;
        inventory_details.extend(self.inventory_details.iter().map(String::as_str));
        match prepared.kind() {
            PreparedContextReadKind::Query => query_pages.push(prepared.raw()),
            PreparedContextReadKind::Inventory => inventory_details.push(prepared.raw()),
        }
        Ok(ProspectiveContextView {
            ledger: prepared.ledger(),
            query_pages,
            evidence: &self.evidence,
            inventory_details,
        })
    }

    /// Publish against the same in-memory acquisition basis. The sink still
    /// owns durable task/lease/cancellation and transaction checks. Query guards
    /// remain live through publication and installation of the prepared buffers.
    pub(super) fn publish_context_read(
        &mut self,
        prepared: PreparedContextRead,
        sink: &mut impl AcquisitionSink,
    ) -> Result<(), HostError> {
        self.validate_prepared_context(&prepared)?;
        // All allocation and UTF-8 conversion precede durable publication.
        match prepared.kind() {
            PreparedContextReadKind::Query => self.query_pages.try_reserve(1),
            PreparedContextReadKind::Inventory => self.inventory_details.try_reserve(1),
        }
        .map_err(|_| HostError::LimitExceeded)?;
        let mut usage = sink.usage().clone();
        if prepared.kind() == PreparedContextReadKind::Query {
            usage.selected_facts = prepared.ledger().selected_facts.len();
        }
        sink.publish_input(prepared.ledger(), usage)?;
        let PreparedContextRead {
            ledger,
            raw,
            kind,
            guards,
            rows,
            ..
        } = prepared;
        self.ledger = ledger;
        self.retain_rows(rows);
        match kind {
            PreparedContextReadKind::Query => self.query_pages.push(raw),
            PreparedContextReadKind::Inventory => self.inventory_details.push(raw),
        }
        drop(guards);
        Ok(())
    }

    pub(super) fn select(
        &self,
        state: &AppState,
        keys: &[FactKey],
    ) -> Result<BTreeMap<FactKey, TaskFactSelection>, HostError> {
        self.select_with(state, keys, NO_ROWS)
    }

    /// [`Self::select`], where a fact may also be one of `fresh`, the stored
    /// copies of a page being prepared at the pinned revision.
    pub(super) fn select_with(
        &self,
        state: &AppState,
        keys: &[FactKey],
        fresh: &ScopedGraph,
    ) -> Result<BTreeMap<FactKey, TaskFactSelection>, HostError> {
        let selections = match &self.scoped_revision {
            // A scope-aware context rereads only the selected facts, at the
            // revision it pinned: a store that moved on is a changed input,
            // and every selected fact must still equal its frozen copy.
            Some(revision) => {
                let read = state
                    .graph
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .read_selected_facts_at_revision(keys, revision, self.graph_limits)
                    .map_err(scoped_graph_error)?;
                if read
                    .nodes
                    .iter()
                    .any(|node| !self.graph.0.contains(node) && !fresh.0.contains(node))
                    || read
                        .edges
                        .iter()
                        .any(|edge| !self.graph.1.contains(edge) && !fresh.1.contains(edge))
                {
                    return Err(HostError::InputChanged);
                }
                read.selections
            }
            None => {
                let read = state
                    .graph
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .read_source_selection_snapshot_bounded(keys, self.graph_limits)
                    .map_err(graph_error)?;
                if read.graph != self.graph {
                    return Err(HostError::InputChanged);
                }
                read.selections
            }
        };
        let selected = selections
            .iter()
            .map(|s| Ok((s.fact.clone(), selection(s)?)))
            .collect::<Result<BTreeMap<_, _>, HostError>>()?;
        for prior in &self.ledger.selected_facts {
            if selected.get(&crate::task_evidence::graph_fact(&prior.fact)) != Some(prior) {
                return Err(HostError::InputChanged);
            }
        }
        Ok(selected)
    }

    pub(super) fn props(&self, fact: &TaskFactKey) -> Result<&serde_json::Value, HostError> {
        self.props_in(fact, NO_ROWS)
    }

    /// [`Self::props`], looking also in `fresh`.
    pub(super) fn props_in<'a>(
        &'a self,
        fact: &TaskFactKey,
        fresh: &'a ScopedGraph,
    ) -> Result<&'a serde_json::Value, HostError> {
        match self.fact_in(fact, fresh) {
            Some(FrozenFact::Node(node)) => Ok(&node.props),
            Some(FrozenFact::Edge(edge)) => Ok(&edge.props),
            None => Err(HostError::InvalidInput),
        }
    }

    /// The frozen copy of `fact`: in this context's graph, or else in
    /// `fresh`, a page being prepared at the pinned revision.
    pub(super) fn fact_in<'a>(
        &'a self,
        fact: &TaskFactKey,
        fresh: &'a ScopedGraph,
    ) -> Option<FrozenFact<'a>> {
        match fact {
            TaskFactKey::Node { id } => self
                .graph
                .0
                .iter()
                .chain(&fresh.0)
                .find(|n| &n.id == id)
                .map(FrozenFact::Node),
            TaskFactKey::Edge {
                source,
                label,
                destination,
            } => self
                .graph
                .1
                .iter()
                .chain(&fresh.1)
                .find(|e| &e.src == source && &e.label == label && &e.dst == destination)
                .map(FrozenFact::Edge),
        }
    }

    /// Keep the stored copies of a published paged page's facts, so later
    /// steps find them without reading the whole graph.
    fn retain_rows(&mut self, rows: ScopedGraph) {
        for node in rows.0 {
            if !self.graph.0.iter().any(|n| n.id == node.id) {
                self.graph.0.push(node);
            }
        }
        for edge in rows.1 {
            if !self
                .graph
                .1
                .iter()
                .any(|e| (&e.src, &e.label, &e.dst) == (&edge.src, &edge.label, &edge.dst))
            {
                self.graph.1.push(edge);
            }
        }
    }
}

fn selection(value: &FactSourceSelection) -> Result<TaskFactSelection, HostError> {
    let (fact_digest, binding) = match &value.state {
        FactSourceState::Absent { fact_digest } => (fact_digest.clone(), None),
        FactSourceState::Present {
            fact_digest,
            binding,
        } => (
            fact_digest.clone(),
            Some(TaskSourceAssociation {
                repo_key: binding.repo_key.clone(),
                receipt_id: binding.receipt_id.clone(),
                emitted_fact_digest: binding.emitted_fact_digest.clone(),
            }),
        ),
        _ => return Err(HostError::InvalidInput),
    };
    Ok(TaskFactSelection {
        fact: crate::task_evidence::task_fact(&value.fact),
        fact_digest,
        binding,
    })
}
fn graph_reference(reference: &FactReference) -> FactKey {
    match reference {
        FactReference::Node { id } => FactKey::Node { id: id.clone() },
        FactReference::Edge {
            source,
            label,
            destination,
        } => FactKey::Edge {
            source: source.clone(),
            label: label.clone(),
            destination: destination.clone(),
        },
    }
}
fn scope(value: &InvestigationScope) -> QueryScope {
    match value {
        InvestigationScope::Node { id } => QueryScope::Node { id: id.clone() },
        InvestigationScope::All => QueryScope::All,
        // An area scopes a task, never a query: a query that names one is
        // refused at validation, and this selects nothing if one arrives.
        InvestigationScope::Area { id } => QueryScope::Node { id: id.clone() },
        InvestigationScope::Neighborhood { anchor, hops } => QueryScope::Neighborhood {
            anchor: anchor.clone(),
            hops: *hops as usize,
        },
    }
}
fn graph_error(error: core_graph::GraphError) -> HostError {
    match error {
        core_graph::GraphError::SnapshotBounds(
            core_graph::SnapshotBoundsError::InvalidSchema
            | core_graph::SnapshotBoundsError::InvalidRow
            | core_graph::SnapshotBoundsError::InvalidJson
            | core_graph::SnapshotBoundsError::InvalidLimits,
        ) => HostError::InvalidInput,
        core_graph::GraphError::SnapshotBounds(_) => HostError::LimitExceeded,
        _ => HostError::Operational,
    }
}
/// Whether a task scope is prepared scope-aware (#1602): a neighborhood
/// (with a valid radius) or an area. `All` keeps the complete-graph read.
pub(super) fn scope_aware(scope: &InvestigationScope) -> Result<bool, HostError> {
    match scope {
        InvestigationScope::Neighborhood { hops, .. } => {
            if *hops == 0 || *hops as usize > context_hub::MAX_NEIGHBORHOOD_HOPS {
                return Err(HostError::InvalidInput);
            }
            Ok(true)
        }
        InvestigationScope::Area { .. } => Ok(true),
        _ => Ok(false),
    }
}
/// [`graph_error`], with a pinned revision that is no longer current read as
/// a changed input rather than an operational failure.
fn scoped_graph_error(error: core_graph::GraphError) -> HostError {
    match error {
        core_graph::GraphError::Generation(core_graph::GenerationError::Changed) => {
            HostError::InputChanged
        }
        error => graph_error(error),
    }
}
fn context_error(error: context_hub::ContextError) -> HostError {
    match error {
        context_hub::ContextError::ResponseBudgetExceeded { .. } => HostError::LimitExceeded,
        _ => HostError::InvalidInput,
    }
}
pub(super) use app_store::investigation_host::bounded_json;

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "context_coverage_tests.rs"]
mod coverage_tests;

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "context_preparation_tests.rs"]
mod preparation_tests;
