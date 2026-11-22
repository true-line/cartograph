// AC-0307 / T-0307: checked logical scoped reservation.
//! Conservative named-owner accounting for scoped workers, not heap/RSS bounds.
//! Existing complete-session allowances are deliberately retained as upper
//! envelopes for scope/coverage/rejection observation, then scoped-only owners
//! are added explicitly. This first composition favors auditability over tightness.
use super::{evidence, session, worker_bound};
use crate::mcp_reads::{scoped_session, worker::WorkerLimits};
use context_hub::logical::{self, count, json, json_bytes, map, set, slots, string_wire, sum};
use core_graph::{Edge, Node};
use std::mem::size_of;

// Shared exact schema inventory includes omission objects and generation metadata.
fn metadata() -> Option<usize> {
    let primary_key = sum(&[4096, 256, slots::<String>(4)?])?;
    Some(super::schema::workspace_bytes()?.max(primary_key))
}
fn fixed_basis() -> Option<usize> {
    let basis = context_hub::scoped::ScopedReadBasis::new(
        core_graph::StoreRevision {
            graph_instance: "f".repeat(32),
            generation: i64::MAX as u64,
        },
        [255; 32],
        context_hub::scoped::ScopedQualifications {
            export_mode: context_hub::scoped::ScopedExportMode::BestEffort,
            registration_digest: [255; 32],
            rejection_digest: [255; 32],
            coverage_digest: [255; 32],
        },
    )
    .ok()?;
    let wire = count(&basis)?;
    // Retained basis/revision, identity Value plus container rebuild, identity
    // wire and query/session/proof ID owners. IDs are counted as actual domains.
    let session_id = "mcp-scoped-session-v1:".len().checked_add(64)?;
    sum(&[
        size_of::<scoped_session::ScopedReadSession>(),
        size_of::<scoped_session::ScopedEvidenceProof>(),
        wire,
        json_bytes(wire)?,
        map::<String, serde_json::Value>(wire.checked_add(1)?)?,
        wire,
        string_wire(session_id)?,
        string_wire(basis.id().len())?,
        string_wire(session_id)?,
        string_wire(basis.id().len())?,
    ])
}
fn digest(limits: core_graph::SnapshotReadLimits) -> Option<usize> {
    let prefix = count(&("cartograph.source.edge-v1", serde_json::Value::Null))?;
    // One selected fact is cloned to Value, canonical containers are rebuilt,
    // and a canonical Vec is allocated. Hashes are computed sequentially.
    sum(&[
        json(limits.max_bytes, limits.max_values)?,
        map::<String, serde_json::Value>(limits.max_values)?,
        slots::<serde_json::Value>(limits.max_values)?,
        limits.max_bytes,
        prefix,
    ])
}
fn associations(limits: core_graph::SnapshotReadLimits) -> Option<usize> {
    let keys = limits
        .max_rows
        .min(core_graph::source::MAX_SOURCE_SELECTION_KEYS);
    let binding_values = keys.checked_mul(sum(&[256, 256, 72, 72])?)?;
    sum(&[
        // Caller key payload and returned binding key payload are separate;
        // sorted key references do not duplicate the underlying strings.
        limits.max_bytes,
        limits.max_bytes,
        slots::<core_graph::source::FactKey>(keys)?,
        set::<&core_graph::source::FactKey>(keys)?,
        slots::<core_graph::source::FactSourceSelection>(keys)?,
        slots::<core_graph::source::SourceBinding>(keys)?,
        binding_values,
        // Each selected omission owns a bounded repo key; SQL cell vectors and
        // decoded strings coexist before authoritative lookup validation.
        keys.checked_mul(256)?,
        slots::<Option<Vec<u8>>>(3)?,
        slots::<String>(3)?,
        256,
        16,
        72,
        // Current SQL BLOBs coexist with decoded binding String fields.
        256,
        256,
        72,
        slots::<Vec<u8>>(3)?,
    ])
}

/// Checked floor; caller must wire this before obtaining the actual worker permit.
/// Complete and scoped dispatch are alternatives, while frozen session state and
/// a new qualification observation coexist during each scoped freshness check.
pub(crate) fn required_bytes(limits: WorkerLimits) -> Option<usize> {
    let full = worker_bound::WorkerReservation::for_limits(limits)?.required_bytes;
    let complete = session::SessionReservation::for_limits(limits)?;
    let coverage = logical::coverage(
        limits.coverage,
        limits.scope.max_sources.get(),
        limits.scope.max_source_registry_bytes.get(),
    )?;
    let r = limits.max_request_frame_bytes.get();
    let w = limits
        .dispatch
        .max_response_bytes
        .get()
        .min(context_hub::MAX_RESPONSE_BYTES);
    let (ids, current_id) = session::request_ids_bytes(limits.dispatch.max_request_ids.get(), r)?;
    let arguments = sum(&[json_bytes(r)?, json_bytes(r)?, r, current_id])?;
    let graph = logical::graph(limits.graph)?;
    // Discovery moves current nodes out of its one window; detail drops the
    // edge probe before reading the joint edge+two-endpoint closure. One graph
    // payload is therefore enough, plus raw current-row decode workspace.
    let selected = sum(&[
        graph,
        limits.graph.max_bytes,
        digest(limits.graph)?,
        associations(limits.graph)?,
        slots::<&Node>(limits.graph.max_rows)?,
        slots::<&Edge>(limits.graph.max_rows)?,
    ])?;
    let candidates = sum(&[
        json_bytes(w)?,
        slots::<scoped_session::ScopedCandidate>(context_hub::discovery::MAX_CANDIDATES)?,
        // Current staged ContextFact may not fit W and is counted before clone.
        graph,
        coverage.retained,
    ])?;
    // Input cursor, decoded current continuation, SQL parameter clone, and
    // output cursor have independent bounded key owners. R covers arbitrary
    // incoming syntax; stored keys are additionally bounded by graph B.
    let key = limits
        .graph
        .max_bytes
        .min(8192usize.checked_add(8192)?.checked_add(256)?);
    let keys = sum(&[
        r,
        key,
        key,
        key,
        slots::<String>(4)?,
        size_of::<scoped_session::ScopedCursor>(),
    ])?;
    // Query identity is pre-counted against context's authoritative cap, not W.
    let query_wire = context_hub::MAX_RESPONSE_BYTES;
    let query = sum(&[
        json_bytes(query_wire)?,
        map::<String, serde_json::Value>(query_wire.checked_add(1)?)?,
        query_wire,
        // Retained normalized matcher query/label payload coexists with its
        // identity DOM; its pre-counted canonical input is <= query_wire.
        query_wire,
        size_of::<context_hub::discovery::DiscoveryQuery>(),
        json_bytes(r)?,
        set::<String>(r.checked_add(1)?)?,
    ])?;
    let scoped_freshness = sum(&[
        metadata()?,
        selected,
        complete.acquisition_peak_bytes,
        fixed_basis()?,
    ])?;
    let evidence = evidence::evidence_peak_bytes(scoped_freshness, limits.evidence)?;
    // Deliberately keep complete retained-session envelope: it exceeds the
    // scoped coverage/rejection roles, but fixed scoped owners are added rather
    // than claiming the differently shaped complete basis covers them.
    let partition = spec::logical_partition_bound(
        flowtracer::TraceGraphBound {
            bytes: limits.graph.max_bytes,
            rows: limits.graph.max_rows,
            values: limits.graph.max_values,
        },
        limits.dispatch.trace,
        limits.dispatch.compile,
    )?;
    let partition_phase = sum(&[
        partition.peak_bytes,
        // Requested root is copied into response metadata after compilation.
        r.min(limits.graph.max_bytes),
        string_wire("mcp-scoped-session-v1:".len().checked_add(64)?)?,
        slots::<String>(2)?,
        // Serialized partition Value and enclosing success Value coexist with
        // the native artifact before response transport ownership transfers.
        json_bytes(w)?,
        json_bytes(w)?,
    ])?;
    // AC-1810: acquisition holds the raw neighborhood, its decode workspace
    // and the reader's node/edge maps and frontiers while the visible clones
    // build one ContextSnapshot (retained facts and indexes plus identity).
    // The raw graph is dropped before the core query runs over the snapshot;
    // the page then gains the cloned coverage and its scoped wrapper Values.
    let rows = limits.graph.max_rows;
    let frontier_ids = sum(&[limits.graph.max_bytes, set::<String>(rows)?])?;
    let neighborhood_snapshot = logical::snapshot(limits.graph)?;
    let acquire = sum(&[
        graph,
        limits.graph.max_bytes,
        sum(&[limits.graph.max_bytes, map::<String, Node>(rows)?])?,
        sum(&[
            limits.graph.max_bytes,
            map::<(String, String, String), Edge>(rows)?,
        ])?,
        frontier_ids.checked_mul(2)?,
        slots::<&Node>(rows)?,
        slots::<&Edge>(rows)?,
        neighborhood_snapshot.retained,
        neighborhood_snapshot.identity_peak,
    ])?;
    let page = sum(&[
        neighborhood_snapshot.retained,
        logical::query_workspace(limits.graph, r)?.peak_bytes,
        coverage.retained,
        coverage.retained,
        json_bytes(w)?,
        json_bytes(w)?,
    ])?;
    let relations = acquire.max(page);
    let scoped = sum(&[
        complete.retained_session_bytes,
        ids,
        arguments,
        fixed_basis()?,
        metadata()?,
        complete.acquisition_peak_bytes, // fresh scope/coverage while frozen state lives
        keys,
        query,
        selected
            .checked_add(candidates)?
            .max(evidence)
            .max(partition_phase)
            .max(relations),
    ])?;
    Some(full.max(scoped))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_floor_covers_complete_floor_and_named_schema_roles() {
        let limits = crate::mcp_reads::worker::tests::limits();
        let bound = required_bytes(limits).unwrap();
        assert!(
            bound
                >= worker_bound::WorkerReservation::for_limits(limits)
                    .unwrap()
                    .required_bytes
        );
        assert!(metadata().unwrap() >= super::super::schema::workspace_bytes().unwrap());
        let mut offered = limits;
        offered.reserved_bytes = std::num::NonZeroUsize::new(1).unwrap();
        assert_eq!(required_bytes(offered), Some(bound));
    }
    #[test]
    fn scoped_roles_are_checked_and_monotone() {
        let limits = crate::mcp_reads::worker::tests::limits();
        let baseline = required_bytes(limits).unwrap();
        let mut larger = limits;
        larger.graph.max_bytes += 1;
        larger.graph.max_rows += 1;
        larger.graph.max_values += 1;
        assert!(required_bytes(larger).unwrap() >= baseline);
        let mut overflow = limits;
        overflow.max_request_frame_bytes = std::num::NonZeroUsize::new(usize::MAX).unwrap();
        assert!(required_bytes(overflow).is_none());
        overflow = limits;
        overflow.graph.max_values = usize::MAX;
        assert!(required_bytes(overflow).is_none());
    }
    #[test]
    fn tiny_transport_caps_do_not_remove_fixed_scoped_metadata() {
        let mut limits = crate::mcp_reads::worker::tests::limits();
        limits.dispatch.max_response_bytes = std::num::NonZeroUsize::new(1).unwrap();
        limits.max_request_frame_bytes = std::num::NonZeroUsize::new(1).unwrap();
        assert!(required_bytes(limits).unwrap() >= metadata().unwrap() + fixed_basis().unwrap());
    }

    #[test]
    fn neighborhood_relations_reserve_snapshot_and_query_payloads() {
        // AC-1810: the scoped relations phase owns a whole visible-neighborhood
        // snapshot and the core query's cloned page alongside it, independent
        // of a tiny response transport.
        let mut limits = crate::mcp_reads::worker::tests::limits();
        limits.dispatch.max_response_bytes = std::num::NonZeroUsize::new(1).unwrap();
        let snapshot = logical::snapshot(limits.graph).unwrap();
        let query =
            logical::query_workspace(limits.graph, limits.max_request_frame_bytes.get()).unwrap();
        assert!(
            required_bytes(limits).unwrap()
                >= snapshot.retained + snapshot.identity_peak.max(query.peak_bytes)
        );
    }

    #[test]
    fn partition_compilation_remains_reserved_with_tiny_response_transport() {
        // AC-0312: rejecting a tiny response still follows closure acquisition
        // and native rendering, so transport size cannot stand in for those owners.
        let mut limits = crate::mcp_reads::worker::tests::limits();
        limits.dispatch.max_response_bytes = std::num::NonZeroUsize::new(1).unwrap();
        let partition = spec::logical_partition_bound(
            flowtracer::TraceGraphBound {
                bytes: limits.graph.max_bytes,
                rows: limits.graph.max_rows,
                values: limits.graph.max_values,
            },
            limits.dispatch.trace,
            limits.dispatch.compile,
        )
        .unwrap();
        assert!(required_bytes(limits).unwrap() >= partition.peak_bytes + metadata().unwrap());
        limits.dispatch.compile.max_bytes = usize::MAX;
        assert!(required_bytes(limits).is_none());
    }
}
