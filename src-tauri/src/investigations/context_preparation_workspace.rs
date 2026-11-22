//! Qualified frozen-context preparation, before any query/source/provider work.
//! Named logical owners only; no allocator slack, native stack or RSS claim.
use super::*;
use crate::mcp_reads::{reservation, scope::ScopeLimits};
use context_hub::logical::{self, count, json_bytes, set, slots, sum};

/// Returns (prepared context retained, preparation peak). `request_frame_bytes`
/// bounds the input from which the normalized AcquisitionRequest is projected;
/// the caller's request/history and all later acquired pages/evidence are separate.
/// Scope limits must be the same limits passed through the detached worker.
pub(crate) fn workspace_bytes(
    scope: ScopeLimits,
    archive_capacity: usize,
    request_frame_bytes: usize,
) -> Option<(usize, usize)> {
    // Source requirements move existing obligation targets; normalization only
    // deduplicates them. Other owned fields project frame-bounded input.
    // Include its typed root in addition to conservative JSON/container owners.
    // The request and frozen context share an immutable effective policy.
    // Account for its owned value and Arc counters; counting it in both named
    // owners is conservative when those owners coexist during preparation.
    let policy_owner = sum(&[slots::<InvestigationPolicy>(1)?, slots::<usize>(2)?])?;
    let request_owner = sum(&[
        json_bytes(request_frame_bytes)?,
        slots::<AcquisitionRequest>(1)?,
        policy_owner,
    ])?;
    let coverage_limits = coverage_preparation_limits(archive_capacity).ok()?;
    let graph_limits = SnapshotReadLimits::default();
    let raw = logical::graph(graph_limits)?;
    let snapshot = logical::snapshot(graph_limits)?;
    let observed_scope = reservation::session::scope_reservation(scope)?;
    let coverage = logical::coverage(
        coverage_limits,
        scope.max_sources.get(),
        scope.max_source_registry_bytes.get(),
    )?;
    let repos = coverage_limits
        .max_records
        .checked_add(scope.max_sources.get())?;
    // Qualification can precede the small manifest admission check. It is
    // bounded by acquisition inputs, not by the eventual accepted proof cap.
    let proof = sum(&[
        json_bytes(coverage.wire_bytes)?,
        slots::<context_hub::coverage::CoverageProof>(1)?,
    ])?;
    let archive = sum(&[
        proof,
        coverage.stored,
        slots::<InvestigationCoverageArchive>(1)?,
    ])?;
    let ledger = sum(&[proof, slots::<InvestigationInputLedger>(1)?, 2 * 128, 64])?;
    let retained = sum(&[
        slots::<FrozenContext>(1)?,
        policy_owner,
        raw,
        snapshot.retained,
        coverage.retained,
        ledger,
        archive,
    ])?;
    // Before/stable scopes coexist with the second observation and with graph
    // reading. Filtering originals consumes stored rows but creates another
    // vector of row slots; the inventory byte payload moves, it is not copied.
    let scope_and_read = sum(&[
        observed_scope.result_bytes,
        observed_scope.result_bytes,
        observed_scope.peak_bytes,
        raw,
        graph_limits.max_bytes,
        coverage.stored,
        coverage.retained,
        coverage.qualification_peak,
    ])?;
    let projection = sum(&[
        raw,
        snapshot.retained,
        snapshot.retained,
        snapshot.identity_peak,
        logical::reference_selection_workspace(graph_limits)?.peak_bytes,
        agents::investigation::PARENT_HISTORY_32768_V1_BYTES,
        slots::<Vec<u8>>(1)?, // bounded supplied-history encoding
    ])?;
    let archive_validation = agents::investigation::coverage_archive_validation_workspace()?;
    let initial = InvestigationInputLedger {
        schema_version: 2,
        coverage: None,
        coverage_reads: vec![],
        artifact_reads: vec![],
        graph_snapshot_id: format!("context-v1:{}", "0".repeat(64)),
        scope_snapshot_id: format!("context-v1:{}", "0".repeat(64)),
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: Some("0".repeat(64)),
        supplied_history_bytes: agents::investigation::PARENT_HISTORY_32768_V1_BYTES,
    };
    // None omits the coverage field; include its key/separator when inserting
    // the separately byte-admitted proof into the initial typed ledger.
    let initial_wire = sum(&[
        MAX_MANIFEST_BYTES,
        count(&initial)?,
        b",\"coverage\":".len(),
    ])?;
    let ledger_validation =
        agents::investigation::ledger_validation_workspace(&INVESTIGATION_V1_POLICY, initial_wire)?
            .max(agents::investigation::ledger_validation_workspace(
                &INVESTIGATION_V2_POLICY,
                initial_wire,
            )?);
    let finalization = sum(&[
        retained,
        proof, // durable_proof before ownership split into ledger/archive
        set::<&str>(repos)?,
        slots::<core_graph::coverage::StoredCoverage>(coverage_limits.max_records)?,
        archive_validation
            .max(ledger_validation)
            .max(MAX_MANIFEST_BYTES),
    ])?;
    // prepare_with_app -> prepare_qualified_with_scope -> from_graph each
    // retains its independent request projection until the child returns.
    let peak = sum(&[
        request_owner.checked_mul(3)?,
        scope_and_read,
        projection.max(finalization),
    ])?;
    Some((retained, peak))
}

/// Additional retained owners after accepted reads. The original graph/archive
/// stays in `workspace_bytes`; preparation/acquisition temporaries are separate.
/// `InvestigationInputLedger::validate_with_policy` bounds combined query/read
/// count; `InvestigationQueryBudget::from_ledger` sums both kinds' response bytes.
/// Count the whole current ledger conservatively, rather than subtracting its
/// initial fields. Query and inventory pages share one admitted byte/item budget.
pub(crate) fn retained_read_bytes(policy: &InvestigationPolicy) -> Option<usize> {
    let limits = policy.limits();
    sum(&[
        json_bytes(MAX_MANIFEST_BYTES)?,
        slots::<InvestigationInputLedger>(1)?,
        MAX_TOTAL_QUERY_BYTES,
        slots::<String>(limits.tool_actions as usize)?,
        // Each accepted evidence ID/source is copied into a validated citation;
        // their combined owned strings cannot exceed the serialized ledger cap.
        MAX_MANIFEST_BYTES,
        limits.evidence_bytes,
        slots::<AgentEvidence>(limits.evidence_items)?,
    ])
}
