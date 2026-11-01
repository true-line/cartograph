//! App-private immutable recovered-context reads. No transport, app startup,
//! provider calls or accepted-proposal/H3 projection lives here.
//!
//! Store admission bounds raw/canonical graph and coverage bytes. It does not
//! bound exact RSS: retained raw facts, projection indexes, temporary identity
//! construction and future artifact caches need separate service admission.

use agents::{DecisionLog, DecisionSelectionError};
use context_hub::coverage::{
    CoverageContext, CoverageQualification, MAX_COVERAGE_IDENTITY_BYTES, MAX_COVERAGE_REPOS,
    QualifiedRequest, QualifiedResponse, ScopedRepo,
};
use context_hub::{ContextError, ContextSnapshot, DiscoveryRequest, DiscoveryResponse};
#[cfg(test)]
use context_hub::{QueryRequest, QueryResponse};
use core_graph::coverage::CoverageReadLimits;
use core_graph::overlay::{OverlayFact, OverlayIdentity, OverlayRead, OverlayStore};
#[cfg(test)]
use core_graph::{Edge, Node};
use core_graph::{
    GraphError, RevisionedSnapshot, SnapshotBoundsError, SnapshotReadLimits, SqliteGraphStore,
    StoreRevision,
};
use serde::Serialize;
use spec::ExportMode;
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

pub(crate) mod artifacts;
pub(crate) mod client_skill;
pub(crate) mod commands;
pub(crate) mod dispatch;
#[cfg(unix)]
pub(crate) mod reservation;
pub(crate) mod scope;
pub(crate) mod scoped_dispatch;
pub(crate) mod scoped_evidence;
pub(crate) mod scoped_session;
#[cfg(unix)]
pub(crate) mod service;
pub(crate) mod task_reservation;
pub(crate) mod worker;

/// Closed source-free failures; never expose storage diagnostics or input text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ReadError {
    #[error("stale_basis")]
    StaleBasis,
    #[error("response_limit")]
    Limit,
    #[error("limit_exceeded")]
    AdmissionLimit,
    #[error("refresh_required")]
    RefreshRequired,
    #[error("storage_unavailable")]
    Storage,
    #[error("artifact_unavailable")]
    ArtifactUnavailable,
    #[error("evidence_unavailable")]
    EvidenceUnavailable,
    #[error("evidence_forgotten")]
    EvidenceForgotten,
    #[error("evidence_omitted")]
    EvidenceOmitted(core_graph::source::ReceiptOmissionKind),
    #[error("evidence_invalid")]
    EvidenceInvalid,
    #[error("invalid_request")]
    InvalidRequest,
}

/// Consistency identities, never authentication credentials or H3 revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReadBasis {
    pub recovered_graph_id: String,
    pub projection_snapshot_id: String,
    pub coverage_id: String,
    /// Projected snapshot plus coverage only. The service must separately bind
    /// cursors to an opaque session and this complete basis (raw ID/mode/policy).
    pub context_id: String,
    pub rejection_decision_digest: String,
    pub registration_basis_digest: String,
    pub export_mode: ExportMode,
    pub h3_curated_available: bool,
    /// Captured mutation token, independent of the base content identity.
    pub base_revision: StoreRevision,
    pub knowledge: KnowledgeBasis,
}

/// Host-captured consistency state, never a caller-supplied current flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum KnowledgeBasis {
    Absent,
    Stale(StoreRevision),
    Current(OverlayIdentity),
}

fn capability_caps(limits: SnapshotReadLimits) -> (usize, usize) {
    (
        limits.max_rows.min(core_graph::overlay::MAX_FACTS),
        limits.max_bytes.min(core_graph::overlay::MAX_BYTES),
    )
}

fn read_knowledge(
    overlay: Option<&Mutex<OverlayStore>>,
    revision: &StoreRevision,
    limits: SnapshotReadLimits,
) -> Result<(KnowledgeBasis, Vec<OverlayFact>, Vec<OverlayFact>), ReadError> {
    let Some(overlay) = overlay else {
        return Ok((KnowledgeBasis::Absent, Vec::new(), Vec::new()));
    };
    let (n, bytes) = capability_caps(limits);
    let captured = overlay
        .lock()
        .map_err(|_| ReadError::Storage)?
        .read_capabilities_and_actors_bounded(revision, n, bytes)
        .map_err(|e| match e {
            core_graph::overlay::OverlayError::SelectionLimit => ReadError::AdmissionLimit,
            _ => ReadError::Storage,
        })?;
    match captured {
        OverlayRead::Absent => Ok((KnowledgeBasis::Absent, Vec::new(), Vec::new())),
        OverlayRead::Stale { base } => {
            // Stored stale metadata is deserialized before publication-name
            // validation; reject malformed revision shape before retaining it.
            if base.generation > i64::MAX as u64
                || base.graph_instance.len() != 32
                || !base
                    .graph_instance
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(ReadError::Storage);
            }
            Ok((KnowledgeBasis::Stale(base), Vec::new(), Vec::new()))
        }
        OverlayRead::Current {
            identity,
            mut facts,
            ..
        } => {
            // One captured selection: Actors move to their own vector and the
            // Capabilities keep the selection's buffer; no fact is copied.
            let actors = facts
                .extract_if(
                    ..,
                    |f| matches!(&f.item, core_graph::overlay::OverlayItem::Node(n) if n.label == "Actor"),
                )
                .collect();
            Ok((KnowledgeBasis::Current(identity), facts, actors))
        }
    }
}

impl ReadBasis {
    /// Conservative wire owners; u64::MAX also covers every valid SQLite generation.
    fn reservation_fixture() -> Self {
        let hash = core_prov::content_hash(&[]);
        Self {
            recovered_graph_id: format!("context-v1:{hash}"),
            projection_snapshot_id: format!("context-v1:{hash}"),
            coverage_id: format!("coverage-context-v1:{hash}"),
            context_id: format!("context-v2:{hash}"),
            rejection_decision_digest: format!("compiler-rejections-v1:{hash}"),
            registration_basis_digest: format!("source-registration-basis-v1:{hash}"),
            export_mode: ExportMode::VerifiedOnly,
            h3_curated_available: false,
            knowledge: KnowledgeBasis::Stale(StoreRevision {
                graph_instance: "f".repeat(32),
                generation: u64::MAX,
            }),
            base_revision: StoreRevision {
                graph_instance: "f".repeat(32),
                generation: u64::MAX,
            },
        }
    }
    #[cfg(unix)]
    fn digest_workspace() -> Option<usize> {
        use context_hub::logical::{count, sum};
        let prefix = "mcp-read-basis-v1";
        let wire = count(&(prefix, Self::reservation_fixture()))?;
        // content_hash returns BLAKE3 hex, not the 32 raw digest bytes. Its
        // String overlaps the owned wire Vec and the formatted returned ID.
        let hash = core_prov::content_hash(&[]).len();
        let identity = sum(&[prefix.len(), ":".len(), hash])?;
        sum(&[
            std::mem::size_of::<Vec<u8>>(),
            wire,
            std::mem::size_of::<String>(),
            hash,
            std::mem::size_of::<String>(),
            identity,
        ])
    }
}

/// One immutable production-policy projection. Raw facts and effective rejected
/// hashes remain private for later selected compilation from exactly this basis.
pub(crate) struct ReadSession {
    basis: ReadBasis,
    graph_limits: SnapshotReadLimits,
    visible_facts: BTreeSet<context_hub::FactReference>,
    base: RevisionedSnapshot,
    capabilities: Vec<OverlayFact>,
    actors: Vec<OverlayFact>,
    rejected_hashes: BTreeSet<String>,
    #[cfg(test)]
    snapshot: Arc<ContextSnapshot>,
    qualified: CoverageContext,
}

impl ReadSession {
    /// Observe policy and registration basis before/after one coherent bounded
    /// graph+coverage read. Observed drift fails rather than mixing revisions.
    /// Brackets cannot exclude ABA changes or create a cross-database transaction.
    /// Scope is trusted app metadata, not deserialized external filesystem input.
    #[cfg(test)]
    pub(crate) fn acquire(
        graph: &Mutex<SqliteGraphStore>,
        decisions: &Mutex<DecisionLog>,
        scope: impl FnMut() -> Result<Vec<ScopedRepo>, ReadError>,
        graph_limits: SnapshotReadLimits,
        coverage_limits: CoverageReadLimits,
        mode: ExportMode,
    ) -> Result<Self, ReadError> {
        Self::acquire_with_overlay(
            graph,
            None,
            decisions,
            scope,
            graph_limits,
            coverage_limits,
            mode,
        )
    }
    pub(crate) fn acquire_with_overlay(
        graph: &Mutex<SqliteGraphStore>,
        overlay: Option<&Mutex<OverlayStore>>,
        decisions: &Mutex<DecisionLog>,
        mut scope: impl FnMut() -> Result<Vec<ScopedRepo>, ReadError>,
        graph_limits: SnapshotReadLimits,
        coverage_limits: CoverageReadLimits,
        mode: ExportMode,
    ) -> Result<Self, ReadError> {
        let rejected_hashes = rejected(decisions)?;
        let before_scope = normalized_scope(scope()?)?;
        let (base, recorded) = graph
            .lock()
            .map_err(|_| ReadError::Storage)?
            .read_snapshot_with_coverage_and_revision_bounded(graph_limits, coverage_limits)
            .map_err(graph_error)?;
        // The graph temporary has dropped; no overlay guard survives this call.
        // This keeps reset's graph -> overlay lock order and the later recheck safe.
        let (knowledge, capabilities, actors) =
            read_knowledge(overlay, &base.revision, graph_limits)?;
        let after_scope = normalized_scope(scope()?)?;
        let after_rejected = rejected(decisions)?;
        if before_scope != after_scope || rejected_hashes != after_rejected {
            return Err(ReadError::StaleBasis);
        }
        // Scope/decision observations run without the graph lock. Refuse any
        // observed graph drift, including same-content and coverage-only writes.
        if graph
            .lock()
            .map_err(|_| ReadError::Storage)?
            .read_revision()
            .map_err(graph_error)?
            != base.revision
        {
            return Err(ReadError::StaleBasis);
        }
        // Drop the temporary raw index before constructing the filtered index.
        // Raw identity must never be replaced by the filtered projection's ID.
        let recovered_graph_id = ContextSnapshot::new(base.graph.0.clone(), base.graph.1.clone())
            .map_err(context_error)?
            .id()
            .to_owned();
        let (nodes, edges) =
            spec::project_recovered_facts(&base.graph.0, &base.graph.1, mode, &rejected_hashes);
        let visible_facts = nodes
            .iter()
            .map(|node| context_hub::FactReference::Node {
                id: node.id.clone(),
            })
            .chain(edges.iter().map(|edge| context_hub::FactReference::Edge {
                source: edge.src.clone(),
                label: edge.label.clone(),
                destination: edge.dst.clone(),
            }))
            .collect();
        let snapshot = Arc::new(
            ContextSnapshot::new(
                nodes.into_iter().cloned().collect(),
                edges.into_iter().cloned().collect(),
            )
            .map_err(context_error)?,
        );
        let coverage =
            CoverageQualification::new(&before_scope, &recorded).map_err(context_error)?;
        let qualified = CoverageContext::from_shared_snapshot(Arc::clone(&snapshot), coverage)
            .map_err(context_error)?;
        let basis = ReadBasis {
            recovered_graph_id,
            projection_snapshot_id: snapshot.id().into(),
            coverage_id: qualified.coverage_id().into(),
            context_id: qualified.context_id().into(),
            rejection_decision_digest: digest("compiler-rejections-v1", &rejected_hashes)?,
            registration_basis_digest: digest("source-registration-basis-v1", &before_scope)?,
            export_mode: mode,
            h3_curated_available: false,
            base_revision: base.revision.clone(),
            knowledge,
        };
        Ok(Self {
            basis,
            graph_limits,
            visible_facts,
            base,
            capabilities,
            actors,
            rejected_hashes,
            #[cfg(test)]
            snapshot,
            qualified,
        })
    }

    pub(crate) fn basis_id(&self) -> Result<String, ReadError> {
        digest("mcp-read-basis-v1", &self.basis)
    }

    pub(crate) fn basis(&self) -> &ReadBasis {
        &self.basis
    }

    #[cfg(test)]
    pub(crate) fn query(
        &self,
        request: QualifiedRequest<QueryRequest>,
    ) -> Result<QualifiedResponse<QueryResponse>, ReadError> {
        self.qualified.query(request).map_err(context_error)
    }

    pub(crate) fn discover(
        &self,
        request: QualifiedRequest<DiscoveryRequest>,
    ) -> Result<QualifiedResponse<DiscoveryResponse>, ReadError> {
        self.qualified.discover(request).map_err(context_error)
    }
}

fn rejected(decisions: &Mutex<DecisionLog>) -> Result<BTreeSet<String>, ReadError> {
    decisions
        .lock()
        .map_err(|_| ReadError::Storage)?
        .read_curation_observation(&[])
        .map(|observation| observation.rejected_hashes)
        .map_err(|error| match error {
            DecisionSelectionError::LimitExceeded => ReadError::Limit,
            _ => ReadError::Storage,
        })
}

fn normalized_scope(mut scope: Vec<ScopedRepo>) -> Result<Vec<ScopedRepo>, ReadError> {
    if scope.len() > MAX_COVERAGE_REPOS {
        return Err(ReadError::Limit);
    }
    for repo in &mut scope {
        for identity in [
            Some(repo.repo_key.as_str()),
            repo.registered_source_id.as_deref(),
            Some(repo.config_hash.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            if identity.len() > MAX_COVERAGE_IDENTITY_BYTES {
                return Err(ReadError::Limit);
            }
            if identity.is_empty() || identity.contains('\0') {
                return Err(ReadError::InvalidRequest);
            }
        }
        if let Some(hints) = &mut repo.layer_hints {
            if hints
                .iter()
                .any(|hint| hint.len() > MAX_COVERAGE_IDENTITY_BYTES)
            {
                return Err(ReadError::Limit);
            }
            hints.sort();
            hints.dedup();
        }
    }
    scope.sort_by(|left, right| left.repo_key.cmp(&right.repo_key));
    if scope
        .windows(2)
        .any(|pair| pair[0].repo_key == pair[1].repo_key)
    {
        return Err(ReadError::InvalidRequest);
    }
    Ok(scope)
}

fn digest(prefix: &str, value: &impl Serialize) -> Result<String, ReadError> {
    let bytes = serde_json::to_vec(&(prefix, value)).map_err(|_| ReadError::Storage)?;
    Ok(format!("{prefix}:{}", core_prov::content_hash(&bytes)))
}

fn graph_error(error: GraphError) -> ReadError {
    match error {
        GraphError::Generation(core_graph::GenerationError::Changed) => ReadError::StaleBasis,
        GraphError::SnapshotBounds(SnapshotBoundsError::InvalidLimits) => ReadError::InvalidRequest,
        GraphError::SnapshotBounds(
            SnapshotBoundsError::RowLimit
            | SnapshotBoundsError::RawByteLimit
            | SnapshotBoundsError::CanonicalByteLimit
            | SnapshotBoundsError::JsonDepthLimit
            | SnapshotBoundsError::JsonValueLimit
            | SnapshotBoundsError::CoverageRecordLimit
            | SnapshotBoundsError::CoverageByteLimit,
        ) => ReadError::Limit,
        _ => ReadError::Storage,
    }
}

fn context_error(error: ContextError) -> ReadError {
    match error {
        ContextError::CursorContextMismatch
        | ContextError::CursorSnapshotMismatch
        | ContextError::CursorSelectionMismatch
        | ContextError::CursorPolicyMismatch => ReadError::StaleBasis,
        ContextError::ResponseBudgetExceeded { .. } => ReadError::Limit,
        ContextError::Serialization(_)
        | ContextError::DuplicateNode(_)
        | ContextError::DuplicateEdge(_)
        | ContextError::InvalidCoverage(_) => ReadError::Storage,
        _ => ReadError::InvalidRequest,
    }
}

#[cfg(test)]
mod tests;

pub(crate) mod evidence;

#[cfg(all(test, unix))]
mod client_smoke_tests;
