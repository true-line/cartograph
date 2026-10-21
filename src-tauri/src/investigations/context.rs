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
use core_graph::{Edge, Node, SnapshotReadLimits};
use core_prov::content_hash;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// One owner holds the original raw graph for exact comparisons and the frozen
/// authorized query projection. Nothing is rebuilt from live context mid-task.
pub(super) struct FrozenContext {
    pub(super) graph: (Vec<Node>, Vec<Edge>),
    pub(super) snapshot: Arc<ContextSnapshot>,
    pub(super) ledger: InvestigationInputLedger,
    pub(super) query_pages: Vec<String>,
    pub(super) evidence: Vec<AgentEvidence>,
    pub(super) coverage_archive: Option<InvestigationCoverageArchive>,
    pub(super) inventory_details: Vec<String>,
    qualified: Option<CoverageContext>,
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

fn coverage_preparation_limits(archive_capacity: usize) -> Result<CoverageReadLimits, HostError> {
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

impl FrozenContext {
    /// Acquisition adapter only. Real V7 intent remains the normalized request4;
    /// this projection is never hashed, stored, or accepted by a start command.
    pub(super) fn completion_acquisition_request(
        request: &obligations::StagedObligationRequest,
    ) -> Result<StartInvestigationRequest, HostError> {
        let request = request.normalized()?;
        let sources = request.obligations.sources()?;
        Ok(StartInvestigationRequest {
            schema_version: 3,
            request_nonce: request.request_nonce,
            specialist_id: if request.specialist_id == "domain-analyst@7" {
                SpecialistId::DomainAnalystV6
            } else {
                SpecialistId::EvidenceAuditorV6
            },
            inspection_requirements: if sources.is_empty() {
                None
            } else {
                Some(normalize_inspection_targets(&sources)?)
            },
            question: request.question,
            scope: request.scope,
            provider_mode: request.provider_mode,
            limit_profile: request.limit_profile,
            expected_graph_revision: request.expected_graph_revision,
            expected_context_id: Some(request.expected_context_id),
            conversation_id: request.conversation_id,
            parent_id: request.parent_id,
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
        if index >= 1024 || self.evidence.len() >= 12 {
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
        let options = match guards.options(state, self, &selected) {
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
        self.ledger.validate()?;
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

    pub(super) fn prepare(
        state: &AppState,
        request: &StartInvestigationRequest,
        history: &[String],
    ) -> Result<Self, HostError> {
        request.validate()?;
        if request.specialist_id.uses_coverage_qualification() {
            return Err(HostError::InvalidInput);
        }
        let graph = state
            .graph
            .lock()
            .map_err(|_| HostError::Operational)?
            .read_snapshot_bounded(SnapshotReadLimits::default())
            .map_err(graph_error)?;
        Self::from_graph(graph, request, history)
    }

    /// The app-backed path brackets registration/plugin discovery around one
    /// bounded graph+inventory read. Legacy preparation keeps its old path.
    pub(super) fn prepare_with_app<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        state: &AppState,
        request: &StartInvestigationRequest,
        history: &[String],
        archive_capacity: Option<usize>,
    ) -> Result<Self, HostError> {
        if !request.specialist_id.uses_coverage_qualification() {
            return Self::prepare(state, request, history);
        }
        let archive_capacity = archive_capacity.ok_or(HostError::InvalidInput)?;
        Self::prepare_qualified_with_scope(state, request, history, archive_capacity, || {
            crate::context::coverage_scope(app, state).map_err(|_| HostError::Operational)
        })
    }

    fn prepare_qualified_with_scope(
        state: &AppState,
        request: &StartInvestigationRequest,
        history: &[String],
        archive_capacity: usize,
        mut scope: impl FnMut() -> Result<Vec<ScopedRepo>, HostError>,
    ) -> Result<Self, HostError> {
        request.validate()?;
        if !request.specialist_id.uses_coverage_qualification() {
            return Err(HostError::InvalidInput);
        }
        let coverage_limits = coverage_preparation_limits(archive_capacity)?;
        let before = scope()?;
        let (graph, recorded) = state
            .graph
            .lock()
            .map_err(|_| HostError::Operational)?
            .read_snapshot_with_coverage_bounded(SnapshotReadLimits::default(), coverage_limits)
            .map_err(graph_error)?;
        let stable = crate::context::stable_scope(&before, scope()?);
        let qualification =
            CoverageQualification::new(&stable, &recorded).map_err(context_error)?;
        let mut prepared = Self::from_graph(graph, request, history)?;
        // Both paths retain the one already-authorized immutable scoped
        // projection from this same bounded graph read.
        let qualified =
            CoverageContext::from_shared_snapshot(Arc::clone(&prepared.snapshot), qualification)
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
        prepared.ledger.validate()?;
        prepared.coverage_archive = Some(archive);
        prepared.qualified = Some(qualified);
        Ok(prepared)
    }

    fn from_graph(
        graph: (Vec<Node>, Vec<Edge>),
        request: &StartInvestigationRequest,
        history: &[String],
    ) -> Result<Self, HostError> {
        let complete = projection(graph.0.clone(), graph.1.clone())?;
        let graph_id = complete.id().to_owned();
        if request
            .expected_graph_revision
            .as_ref()
            .is_some_and(|expected| expected != &graph_id)
        {
            return Err(HostError::InputChanged);
        }
        let snapshot = Arc::new(Self::scoped_snapshot(&graph, &request.scope, complete)?);
        let history_bytes = if history.is_empty() {
            Vec::new()
        } else {
            bounded_json(history, 16 * 1024)?
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
        ledger.validate()?;
        Ok(Self {
            graph,
            snapshot,
            ledger,
            query_pages: vec![],
            evidence: vec![],
            coverage_archive: None,
            inventory_details: vec![],
            qualified: None,
        })
    }

    pub(super) fn scoped_snapshot(
        graph: &(Vec<Node>, Vec<Edge>),
        requested: &InvestigationScope,
        complete: ContextSnapshot,
    ) -> Result<ContextSnapshot, HostError> {
        Ok(match requested {
            InvestigationScope::All => complete,
            InvestigationScope::Neighborhood { .. } => {
                // Reuse the service's actual scope selection rather than a
                // subtly different application-specific graph traversal.
                let mut references = BTreeSet::new();
                let mut cursor = None;
                loop {
                    let page = complete
                        .query(QueryRequest {
                            scope: scope(requested),
                            kind: None,
                            labels: vec![],
                            max_facts: context_hub::MAX_FACTS,
                            max_bytes: context_hub::MAX_RESPONSE_BYTES,
                            cursor,
                        })
                        .map_err(context_error)?;
                    references.extend(page.facts.into_iter().map(|f| f.reference));
                    let Some(next) = page.next_cursor else {
                        break;
                    };
                    cursor = Some(next);
                }
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

    pub(super) fn query(
        &mut self,
        state: &AppState,
        query: InvestigationQuery,
        sink: &mut impl AcquisitionSink,
    ) -> Result<(), HostError> {
        query.validate()?;
        self.ledger.validate()?;
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
            _ => return Err(HostError::InvalidInput),
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
            match &self.qualified {
                Some(qualified) => qualified
                    .query(QualifiedRequest {
                        request,
                        context_id: Some(qualified.context_id().to_owned()),
                    })
                    .map(InvestigationPageContext::Qualified),
                None => self
                    .snapshot
                    .query(request)
                    .map(InvestigationPageContext::Legacy),
            }
            .map_err(context_error)
        };
        let prior_keys = self
            .ledger
            .selected_facts
            .iter()
            .map(|s| crate::task_evidence::graph_fact(&s.fact))
            .collect::<BTreeSet<_>>();
        let page = query_page(query.max_facts)?;
        let mut keys = prior_keys.clone();
        keys.extend(
            page.page()
                .facts
                .iter()
                .map(|f| graph_reference(&f.reference)),
        );
        if keys.len() > MAX_SELECTED_FACTS {
            return Err(HostError::LimitExceeded);
        }
        let keys = keys.into_iter().collect::<Vec<_>>();
        let preliminary = self.select(state, &keys)?;
        let guards = EvidenceGuards::acquire(state, &preliminary)?;
        let authoritative = self.select(state, &keys)?;
        if authoritative != preliminary {
            return Err(HostError::InputChanged);
        }
        let mut options = Vec::new();
        for fact in &page.page().facts {
            let selected = authoritative
                .get(&graph_reference(&fact.reference))
                .ok_or(HostError::InvalidInput)?;
            options.push(guards.options(state, self, selected)?);
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
                    response.context = query_page(response.context.page().facts.len() - 1)?;
                    response
                        .evidence_options
                        .truncate(response.context.page().facts.len());
                }
                Err(error) => return Err(error),
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
        InvestigationQueryBudget::from_ledger(&ledger)?.admit(raw.len())?;
        ledger.queries.push(InvestigationQueryManifest {
            query,
            response_hash: content_hash(&raw),
            response_bytes: raw.len(),
            returned_facts: response.context.page().facts.len(),
            total_selected: response.context.page().total_selected,
            has_more: response.context.page().next_cursor.is_some(),
        });
        ledger.validate()?;
        let mut usage = sink.usage().clone();
        usage.selected_facts = ledger.selected_facts.len();
        sink.publish_input(&ledger, usage)?;
        // The guard remains live through the durable receipt-reference commit.
        self.ledger = ledger;
        self.query_pages
            .push(String::from_utf8(raw).map_err(|_| HostError::InvalidInput)?);
        Ok(())
    }

    /// Acquire exact original inventory detail without consulting graph or
    /// source storage. Its response is untrusted coverage data, never a citation.
    pub(super) fn read_inventory(
        &mut self,
        request: InvestigationCoverageRead,
        sink: &mut impl AcquisitionSink,
    ) -> Result<(), HostError> {
        self.ledger.validate()?;
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
            return Err(HostError::InvalidInput);
        }
        let detail = archive.read(&request)?;
        let raw = bounded_json(&detail, request.max_bytes)?;
        InvestigationQueryBudget::from_ledger(&self.ledger)?.admit(raw.len())?;
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
        ledger.validate()?;
        let raw = String::from_utf8(raw).map_err(|_| HostError::InvalidInput)?;
        sink.publish_input(&ledger, sink.usage().clone())?;
        self.ledger = ledger;
        self.inventory_details.push(raw);
        Ok(())
    }

    pub(super) fn select(
        &self,
        state: &AppState,
        keys: &[FactKey],
    ) -> Result<BTreeMap<FactKey, TaskFactSelection>, HostError> {
        let read = state
            .graph
            .lock()
            .map_err(|_| HostError::Operational)?
            .read_source_selection_snapshot_bounded(keys, SnapshotReadLimits::default())
            .map_err(graph_error)?;
        if read.graph != self.graph {
            return Err(HostError::InputChanged);
        }
        let selected = read
            .selections
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
        match fact {
            TaskFactKey::Node { id } => self.graph.0.iter().find(|n| &n.id == id).map(|n| &n.props),
            TaskFactKey::Edge {
                source,
                label,
                destination,
            } => self
                .graph
                .1
                .iter()
                .find(|e| &e.src == source && &e.label == label && &e.dst == destination)
                .map(|e| &e.props),
        }
        .ok_or(HostError::InvalidInput)
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
        InvestigationScope::All => QueryScope::All,
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
fn context_error(error: context_hub::ContextError) -> HostError {
    match error {
        context_hub::ContextError::ResponseBudgetExceeded { .. } => HostError::LimitExceeded,
        _ => HostError::InvalidInput,
    }
}
pub(super) fn bounded_json(
    value: &(impl Serialize + ?Sized),
    max: usize,
) -> Result<Vec<u8>, HostError> {
    struct Limited {
        bytes: Vec<u8>,
        max: usize,
    }
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.max.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("investigation byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Limited { bytes: vec![], max };
    serde_json::to_writer(&mut writer, value).map_err(|_| HostError::LimitExceeded)?;
    Ok(writer.bytes)
}

#[cfg(test)]
#[path = "context_coverage_tests.rs"]
mod coverage_tests;
