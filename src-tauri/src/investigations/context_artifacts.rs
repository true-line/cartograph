//! In-app artifact reads (ADR-0089, AC-1814): compile one official artifact
//! from the investigation's frozen projection and page it under the same
//! byte-bounded, UTF-8-safe rule as the MCP artifact read. The store is read
//! only at the task's pinned revision, or a later one with an identical graph,
//! so a page always describes the frozen basis.
use super::*;
use agents::investigation::{
    InvestigationArtifactPage, InvestigationArtifactRead, InvestigationArtifactReadManifest,
};
use std::num::NonZeroUsize;

/// Trace bounds for an in-app compilation. A trace over them is refused
/// with its limit, never compiled from a partial flow set.
pub(super) const ARTIFACT_TRACE_LIMITS: flowtracer::TraceLimits = flowtracer::TraceLimits {
    max_flows: 256,
    max_hops: 256,
    max_output_bytes: 4 * 1024 * 1024,
};
/// Compile bounds for an in-app compilation. A larger artifact is refused,
/// never returned as a truncated document.
pub(super) const ARTIFACT_COMPILE_LIMITS: spec::SelectedCompileLimits =
    spec::SelectedCompileLimits {
        max_bytes: 8 * 1024 * 1024,
        adr: spec::AdrLimits {
            max_rows: 512,
            max_output_bytes: 2 * 1024 * 1024,
        },
    };

/// Artifacts compiled from the graph alone: the agent core's closed list
/// (AC-1815), each of which must be in the MCP-readable catalogue.
pub(crate) fn investigation_readable(id: &str) -> bool {
    agents::investigation::INVESTIGATION_ARTIFACT_IDS.contains(&id)
        && crate::mcp_reads::artifacts::readable_catalog()
            .iter()
            .any(|artifact| artifact.id == id)
}

/// Compile one graph-only artifact from these facts.
pub(super) fn compile_frozen(
    graph: &(Vec<Node>, Vec<Edge>),
    graph_limits: SnapshotReadLimits,
    id: &str,
) -> Result<spec::SpecArtifact, HostError> {
    if !investigation_readable(id) {
        return Err(HostError::InvalidInput);
    }
    let flows = flowtracer::trace_bounded(&graph.0, &graph.1, ARTIFACT_TRACE_LIMITS)
        .map_err(|_| HostError::LimitExceeded)?;
    let security = spec::security_projection_bound(
        graph_limits.max_bytes,
        graph_limits.max_rows,
        graph_limits.max_values,
    )
    .ok_or(HostError::LimitExceeded)?;
    let outcome = spec::compile_spec_selected_with_security_supplied(
        &graph.0,
        &graph.1,
        &flows,
        // The frozen projection is what the agent already reads through
        // query_context; the artifact is compiled over exactly those facts.
        spec::ExportMode::BestEffort,
        &BTreeSet::new(),
        &[id],
        ARTIFACT_COMPILE_LIMITS,
        (
            spec::SecurityLimits {
                max_rows: security.rows,
                max_output_bytes: security.bytes,
            },
            spec::Supplied::default(),
        ),
    )
    .map_err(|_| HostError::InvalidInput)?
    .into_iter()
    .next()
    .ok_or(HostError::InvalidInput)?;
    match outcome {
        spec::SelectedArtifactOutcome::Produced(artifact) if artifact.id == id => Ok(artifact),
        spec::SelectedArtifactOutcome::Produced(_) => Err(HostError::InvalidInput),
        spec::SelectedArtifactOutcome::Limited { .. } => Err(HostError::LimitExceeded),
    }
}

/// The largest page of `artifact` from `request.offset` whose canonical
/// encoding fits `request.max_bytes`.
pub(super) fn page(
    artifact: &spec::SpecArtifact,
    request: &InvestigationArtifactRead,
    complete_system: bool,
) -> Result<InvestigationArtifactPage, HostError> {
    let page_at = |end: usize| InvestigationArtifactPage {
        schema_version: 1,
        context_id: request.context_id.clone(),
        artifact_id: artifact.id.clone(),
        schema: artifact.schema.clone(),
        content_id: artifact.content_id.clone(),
        offset: request.offset,
        next_offset: (end < artifact.content.len()).then_some(end),
        total_content_bytes: artifact.content.len(),
        complete_system,
        content: artifact.content[request.offset..end].to_owned(),
    };
    let max_content = NonZeroUsize::new(request.max_bytes).ok_or(HostError::InvalidInput)?;
    let end = crate::mcp_reads::artifacts::fitting_end(
        &artifact.content,
        request.offset,
        max_content,
        |end| bounded_json(&page_at(end), request.max_bytes).is_ok(),
    )
    .map_err(|error| match error {
        crate::mcp_reads::ReadError::Limit => HostError::LimitExceeded,
        _ => HostError::InvalidInput,
    })?;
    Ok(page_at(end))
}

impl FrozenContext {
    /// Acquire one artifact page and publish it after the coverage bodies.
    /// Its content is untrusted compiled text, never a citation. The worker
    /// publishes through metadata admission instead; tests drive this.
    #[cfg(test)]
    pub(super) fn read_artifact(
        &mut self,
        state: &AppState,
        request: InvestigationArtifactRead,
        sink: &mut impl AcquisitionSink,
    ) -> Result<(), HostError> {
        let prepared = self
            .prepare_artifact_with_diagnostic(state, request)
            .map_err(ContextReadPreparationError::into_host_error)?;
        self.publish_context_read(prepared, sink)
    }

    /// Prepare one artifact page against the frozen basis without publishing
    /// it or mutating the acquisition state.
    pub(in crate::investigations) fn prepare_artifact_with_diagnostic(
        &self,
        state: &AppState,
        request: InvestigationArtifactRead,
    ) -> Result<PreparedContextRead, ContextReadPreparationError> {
        self.ledger.validate_with_policy(&self.policy)?;
        let proof = self
            .ledger
            .coverage
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        if request.context_id != proof.context_id || !request.validate_shape() {
            return Err(HostError::InvalidInput.into());
        }
        let (graph, complete_system) = self.artifact_basis(state)?;
        let artifact = compile_frozen(&graph, self.graph_limits, &request.artifact_id)?;
        // A continuation of other content is stale, never a page of this one.
        if request
            .expected_content_id
            .as_ref()
            .is_some_and(|expected| expected != &artifact.content_id)
        {
            return Err(HostError::InputChanged.into());
        }
        let page = page(&artifact, &request, complete_system)?;
        let raw = serde_json::to_string(&page).map_err(|_| HostError::InvalidInput)?;
        InvestigationQueryBudget::from_ledger(&self.ledger)
            .and_then(|budget| budget.admit(raw.len()))
            .map_err(|_| ContextReadPreparationError::CumulativeQueryAndInventoryBytes)?;
        let mut ledger = self.ledger.clone();
        ledger.revision = ledger
            .revision
            .checked_add(1)
            .ok_or(HostError::LimitExceeded)?;
        ledger
            .artifact_reads
            .push(InvestigationArtifactReadManifest {
                request,
                content_id: page.content_id,
                response_hash: content_hash(raw.as_bytes()),
                response_bytes: raw.len(),
                next_offset: page.next_offset,
            });
        ledger.validate_with_policy(&self.policy)?;
        Ok(PreparedContextRead {
            base_ledger: self.ledger.clone(),
            policy_profile: self.policy.profile(),
            ledger,
            raw,
            kind: PreparedContextReadKind::Artifact,
            guards: None,
            rows: Default::default(),
        })
    }
}

/// The facts an artifact compiles from, held or read at the pinned revision.
type ArtifactGraph<'a> = std::borrow::Cow<'a, (Vec<Node>, Vec<Edge>)>;

impl FrozenContext {
    /// The facts an artifact is compiled from and whether they are the whole
    /// system. A whole-system task holds no graph (AC-1058), so it reads the
    /// complete graph at its pinned revision, or at a later one only when the
    /// complete-graph identity is unchanged; any other change is
    /// [`HostError::InputChanged`]. A scope-aware task compiles its held
    /// scope facts, labeled as not the whole system even when its scope
    /// happens to hold every current fact. A complete-graph
    /// preparation of a narrower scope has no held scope graph and refuses.
    fn artifact_basis(&self, state: &AppState) -> Result<(ArtifactGraph<'_>, bool), HostError> {
        let whole = self.ledger.scope_snapshot_id == self.ledger.graph_snapshot_id;
        match (&self.snapshot, &self.scoped_revision) {
            (ScopeFacts::Paged { id }, _) => {
                let revision = self.paged_revision(state, id)?;
                let read = state
                    .graph
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .read_snapshot_at_revision_bounded(Some(&revision), self.graph_limits)
                    .map_err(scoped_graph_error)?;
                Ok((std::borrow::Cow::Owned(read.graph), true))
            }
            (ScopeFacts::Held(_), Some(_)) => Ok((std::borrow::Cow::Borrowed(&self.graph), false)),
            (ScopeFacts::Held(_), None) if whole => {
                Ok((std::borrow::Cow::Borrowed(&self.graph), true))
            }
            (ScopeFacts::Held(_), None) => Err(HostError::InvalidInput),
        }
    }
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "context_artifacts_tests.rs"]
mod tests;
