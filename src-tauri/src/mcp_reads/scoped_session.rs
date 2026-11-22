//! Bounded revision-bound MCP reads; no complete graph or artifact identity.
use super::{ReadError, context_error, graph_error, normalized_scope, rejected};
use agents::DecisionLog;
use context_hub::{
    ContextFact,
    coverage::{CoverageQualification, ScopedRepo},
    discovery::{DiscoveryMatcher, DiscoveryQuery, MAX_CANDIDATES, MatchReason},
    scoped::{ScopedExportMode, ScopedQualifications, ScopedReadBasis},
};
use core_graph::{
    SnapshotReadLimits, SqliteGraphStore, StoreRevision,
    coverage::CoverageReadLimits,
    scoped::{NodeWindowRequest, Remaining, SelectedFacts},
    source::FactKey,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Mutex};

pub(crate) struct ScopedReadContext<'a> {
    pub graph: &'a Mutex<SqliteGraphStore>,
    pub decisions: &'a Mutex<DecisionLog>,
    pub scope: &'a mut dyn FnMut() -> Result<Vec<ScopedRepo>, ReadError>,
}
pub(crate) struct ScopedReadSession {
    basis_id: String,
    revision: StoreRevision,
    qualifications: ScopedQualifications,
    coverage: CoverageQualification,
    rejected: BTreeSet<String>,
    graph_limits: SnapshotReadLimits,
    coverage_limits: CoverageReadLimits,
    mode: spec::ExportMode,
}
/// Constructible only after current qualification and selected visibility checks.
pub(crate) struct ScopedEvidenceProof {
    basis_id: String,
    revision: StoreRevision,
    graph_limits: SnapshotReadLimits,
    fact: FactKey,
}
impl ScopedEvidenceProof {
    pub(crate) fn basis_id(&self) -> &str {
        &self.basis_id
    }
    pub(crate) fn revision(&self) -> &StoreRevision {
        &self.revision
    }
    pub(crate) fn graph_limits(&self) -> SnapshotReadLimits {
        self.graph_limits
    }
    pub(crate) fn fact(&self) -> &FactKey {
        &self.fact
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScopedCursor {
    pub query_basis_id: String,
    /// Caller-selected exclusive raw primary-key lower bound. This is a scan
    /// position, not proof that any earlier key was examined or returned.
    pub after_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScopedDiscoveryRequest {
    pub query: DiscoveryQuery,
    pub labels: Vec<String>,
    pub max_scan_rows: usize,
    pub max_candidates: usize,
    pub max_bytes: usize,
    pub cursor: Option<ScopedCursor>,
}
#[derive(Debug, Serialize)]
pub(crate) struct ScopedCandidate {
    pub reference: context_hub::FactReference,
    pub label: String,
    pub source: context_hub::discovery::CandidateSource,
    pub confidence_tier: core_prov::ConfidenceTier,
    pub provenance_problem: Option<context_hub::ProvenanceProblem>,
    pub tier: Option<core_prov::Tier>,
    pub extractor_id: Option<String>,
    pub content_hash: Option<String>,
    pub method: Option<core_prov::ProducingMethod>,
    pub match_reason: MatchReason,
    pub outgoing_edges: Option<usize>,
    pub incoming_edges: Option<usize>,
}
#[derive(Debug, Serialize)]
pub(crate) struct ScopedDiscoveryPage {
    pub session_basis_id: String,
    pub query_basis_id: String,
    pub revision: StoreRevision,
    pub coverage: CoverageQualification,
    /// Raw rows whose matching policy was processed for this page.
    pub scanned_rows: usize,
    /// Storage rows decoded into the admitted window, including unprocessed tail.
    pub loaded_rows: usize,
    /// More/Exhausted describes only the raw tail after this request's lower
    /// bound; even Exhausted never establishes full-graph or semantic absence.
    pub remaining: Remaining,
    pub complete_system: bool,
    pub remaining_rows: Option<usize>,
    pub candidates: Vec<ScopedCandidate>,
    pub next_cursor: Option<ScopedCursor>,
}
#[derive(Debug, Serialize)]
pub(crate) struct ScopedDetail {
    pub session_basis_id: String,
    pub revision: StoreRevision,
    pub coverage: CoverageQualification,
    pub complete_system: bool,
    pub absence_claim: bool,
    pub fact: Option<ContextFact>,
}

fn hash<T: Serialize>(value: &T) -> Result<[u8; 32], ReadError> {
    // Inputs are host-admitted scope/rejection/coverage owners, not caller JSON.
    // Count before allocating the identity wire; no response is serialized here.
    let length = context_hub::logical::count(value).ok_or(ReadError::Limit)?;
    let mut wire = Vec::with_capacity(length);
    serde_json::to_writer(&mut wire, value).map_err(|_| ReadError::Storage)?;
    let hex = core_prov::content_hash(&wire);
    let mut result = [0; 32];
    for (i, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| ReadError::Storage)?;
    }
    Ok(result)
}
fn graph<T>(result: Result<T, core_graph::GraphError>) -> Result<T, ReadError> {
    result.map_err(|e| match e {
        core_graph::GraphError::Generation(core_graph::GenerationError::Changed) => {
            ReadError::StaleBasis
        }
        other => graph_error(other),
    })
}
fn observe(
    ctx: &mut ScopedReadContext<'_>,
    coverage_limits: CoverageReadLimits,
    mode: spec::ExportMode,
) -> Result<
    (
        StoreRevision,
        ScopedQualifications,
        CoverageQualification,
        BTreeSet<String>,
    ),
    ReadError,
> {
    let rejected_before = rejected(ctx.decisions)?;
    let before = normalized_scope((ctx.scope)()?)?;
    let (revision, records) = graph(
        ctx.graph
            .lock()
            .map_err(|_| ReadError::Storage)?
            .read_coverage_records_with_revision_bounded(coverage_limits),
    )?;
    let after = normalized_scope((ctx.scope)()?)?;
    let rejected_after = rejected(ctx.decisions)?;
    if before != after || rejected_before != rejected_after {
        return Err(ReadError::StaleBasis);
    }
    let coverage = CoverageQualification::new(&before, &records).map_err(context_error)?;
    let quals = ScopedQualifications {
        export_mode: match mode {
            spec::ExportMode::VerifiedOnly => ScopedExportMode::VerifiedOnly,
            spec::ExportMode::BestEffort => ScopedExportMode::BestEffort,
        },
        registration_digest: hash(&before)?,
        rejection_digest: hash(&rejected_before)?,
        coverage_digest: hash(&coverage)?,
    };
    let current = graph(
        ctx.graph
            .lock()
            .map_err(|_| ReadError::Storage)?
            .read_revision(),
    )?;
    if current != revision {
        return Err(ReadError::StaleBasis);
    }
    Ok((revision, quals, coverage, rejected_before))
}
impl ScopedReadSession {
    pub(super) fn validate_graph_limits(graph_limits: SnapshotReadLimits) -> Result<(), ReadError> {
        if graph_limits.max_rows == 0
            || graph_limits.max_rows > SnapshotReadLimits::MAX_ROWS
            || graph_limits.max_bytes == 0
            || graph_limits.max_bytes > SnapshotReadLimits::MAX_BYTES
            || graph_limits.max_depth == 0
            || graph_limits.max_depth > SnapshotReadLimits::MAX_DEPTH
            || graph_limits.max_values == 0
            || graph_limits.max_values > SnapshotReadLimits::MAX_VALUES
        {
            return Err(ReadError::InvalidRequest);
        }
        Ok(())
    }

    pub(crate) fn acquire(
        ctx: &mut ScopedReadContext<'_>,
        graph_limits: SnapshotReadLimits,
        coverage_limits: CoverageReadLimits,
        mode: spec::ExportMode,
    ) -> Result<Self, ReadError> {
        Self::validate_graph_limits(graph_limits)?;
        let (revision, qualifications, coverage, rejected) = observe(ctx, coverage_limits, mode)?;
        let id = hash(&("mcp-scoped-session-v1", &revision, &qualifications))?;
        let basis_id = format!(
            "mcp-scoped-session-v1:{}",
            id.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        Ok(Self {
            basis_id,
            revision,
            qualifications,
            coverage,
            rejected,
            graph_limits,
            coverage_limits,
            mode,
        })
    }
    pub(crate) fn basis_id(&self) -> &str {
        &self.basis_id
    }
    pub(crate) fn revision(&self) -> &StoreRevision {
        &self.revision
    }
    pub(crate) fn graph_limits(&self) -> SnapshotReadLimits {
        self.graph_limits
    }
    pub(crate) fn coverage(&self) -> &CoverageQualification {
        &self.coverage
    }
    pub(crate) fn check_current(&self, ctx: &mut ScopedReadContext<'_>) -> Result<(), ReadError> {
        let (revision, quals, _, _) = observe(ctx, self.coverage_limits, self.mode)?;
        if revision != self.revision || quals != self.qualifications {
            return Err(ReadError::StaleBasis);
        }
        Ok(())
    }
    fn require_basis(&self, expected: &str) -> Result<(), ReadError> {
        if expected != self.basis_id {
            Err(ReadError::StaleBasis)
        } else {
            Ok(())
        }
    }
    fn visible_selected(
        &self,
        ctx: &ScopedReadContext<'_>,
        key: &FactKey,
    ) -> Result<SelectedFacts, ReadError> {
        let mut selected = graph(
            ctx.graph
                .lock()
                .map_err(|_| ReadError::Storage)?
                .read_selected_facts_at_revision(
                    std::slice::from_ref(key),
                    &self.revision,
                    self.graph_limits,
                ),
        )?;
        if let Some(edge) = selected.edges.first() {
            let keys = [
                key.clone(),
                FactKey::Node {
                    id: edge.src.clone(),
                },
                FactKey::Node {
                    id: edge.dst.clone(),
                },
            ];
            let expected = if edge.src == edge.dst { 1 } else { 2 };
            // Release the probe before acquiring the complete edge/endpoint
            // closure under one shared SQL preallocation/JSON admission budget.
            drop(selected);
            selected = graph(
                ctx.graph
                    .lock()
                    .map_err(|_| ReadError::Storage)?
                    .read_selected_facts_at_revision(&keys, &self.revision, self.graph_limits),
            )?;
            if selected.nodes.len() != expected || selected.edges.len() != 1 {
                return Err(ReadError::Limit);
            }
        }
        let (nodes, edges) = spec::project_recovered_facts(
            &selected.nodes,
            &selected.edges,
            self.mode,
            &self.rejected,
        );
        let visible = match key {
            FactKey::Node { id } => nodes.iter().any(|n| n.id == *id),
            FactKey::Edge {
                source,
                label,
                destination,
            } => edges
                .iter()
                .any(|e| e.src == *source && e.label == *label && e.dst == *destination),
        };
        if !visible {
            selected.nodes.clear();
            selected.edges.clear();
        }
        Ok(selected)
    }
    pub(crate) fn detail(
        &self,
        ctx: &mut ScopedReadContext<'_>,
        expected: &str,
        key: &FactKey,
        max_bytes: usize,
    ) -> Result<ScopedDetail, ReadError> {
        self.require_basis(expected)?;
        self.check_current(ctx)?;
        let selected = self.visible_selected(ctx, key)?;
        let fact = match key {
            FactKey::Node { .. } => selected
                .nodes
                .into_iter()
                .next()
                .map(ContextFact::from_node),
            FactKey::Edge { .. } => selected
                .edges
                .into_iter()
                .next()
                .map(ContextFact::from_edge),
        };
        self.check_current(ctx)?;
        #[derive(Serialize)]
        struct DetailView<'a> {
            session_basis_id: &'a str,
            revision: &'a StoreRevision,
            coverage: &'a CoverageQualification,
            complete_system: bool,
            absence_claim: bool,
            fact: &'a Option<ContextFact>,
        }
        admission::fits(
            &DetailView {
                session_basis_id: &self.basis_id,
                revision: &self.revision,
                coverage: &self.coverage,
                complete_system: false,
                absence_claim: false,
                fact: &fact,
            },
            max_bytes,
        )?;
        let response = ScopedDetail {
            session_basis_id: self.basis_id.clone(),
            revision: self.revision.clone(),
            coverage: self.coverage.clone(),
            complete_system: false,
            absence_claim: false,
            fact,
        };
        Ok(response)
    }
    pub(crate) fn authorize_evidence(
        &self,
        ctx: &mut ScopedReadContext<'_>,
        expected: &str,
        key: &FactKey,
    ) -> Result<ScopedEvidenceProof, ReadError> {
        self.require_basis(expected)?;
        self.check_current(ctx)?;
        let selected = self.visible_selected(ctx, key)?;
        if selected.nodes.is_empty() && selected.edges.is_empty() {
            return Err(ReadError::InvalidRequest);
        }
        self.check_current(ctx)?;
        Ok(ScopedEvidenceProof {
            basis_id: self.basis_id.clone(),
            revision: self.revision.clone(),
            graph_limits: self.graph_limits,
            fact: key.clone(),
        })
    }
}
mod admission;
mod discovery;
mod partition;
mod relations;
pub(crate) use relations::ScopedRelationsRequest;
#[cfg(test)]
mod tests;
