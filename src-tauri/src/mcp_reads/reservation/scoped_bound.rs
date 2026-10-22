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

// Exact private schema admission ceilings in core-graph generation/source/coverage.
// These are schema-object counts, not graph or policy ceilings.
const GENERATION_OBJECTS: usize = 16;
const SOURCE_OBJECTS: usize = 11;
const SOURCE_GENERATION_OBJECTS: usize = 12;
const COVERAGE_OBJECTS: usize = 2;
const COVERAGE_GENERATION_OBJECTS: usize = 3;
const SCHEMA_NAME_BYTES: usize = 256;
const SCHEMA_TYPE_BYTES: usize = 16;
const SCHEMA_SQL_BYTES: usize = 4096;

fn schema_map(objects: usize) -> Option<usize> {
    sum(&[
        objects.checked_mul(sum(&[
            SCHEMA_NAME_BYTES,
            SCHEMA_TYPE_BYTES,
            SCHEMA_SQL_BYTES,
        ])?)?,
        map::<String, (String, String)>(objects)?,
    ])
}
fn metadata() -> Option<usize> {
    let expected_generation = sum(&[
        GENERATION_OBJECTS.checked_mul(sum(&[
            SCHEMA_NAME_BYTES,
            SCHEMA_TYPE_BYTES,
            SCHEMA_SQL_BYTES,
        ])?)?,
        slots::<(String, String, String)>(GENERATION_OBJECTS)?,
    ])?;
    let generation = sum(&[
        expected_generation,
        schema_map(GENERATION_OBJECTS)?,
        // Current expected type/SQL clones during exact comparison.
        SCHEMA_TYPE_BYTES,
        SCHEMA_SQL_BYTES,
        slots::<String>(2)?,
        slots::<(i64, i64, i64)>(2)?,
    ])?;
    // An owned schema map can remain live while generation is revalidated;
    // source and coverage checks run sequentially, not simultaneously.
    let source = sum(&[
        schema_map(SOURCE_OBJECTS.checked_add(SOURCE_GENERATION_OBJECTS)?)?,
        schema_map(SOURCE_OBJECTS)?,
        generation,
    ])?;
    let coverage = sum(&[
        schema_map(COVERAGE_OBJECTS.checked_add(COVERAGE_GENERATION_OBJECTS)?)?,
        schema_map(COVERAGE_OBJECTS)?,
        generation,
    ])?;
    // Ordinary-table preflight admits SQL <=4096 before reading xinfo. One
    // current xinfo/index row plus the bounded index name is owned at a time.
    let primary_key = sum(&[SCHEMA_SQL_BYTES, SCHEMA_NAME_BYTES, slots::<String>(4)?])?;
    Some(source.max(coverage).max(generation).max(primary_key))
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
    let scoped = sum(&[
        complete.retained_session_bytes,
        ids,
        arguments,
        fixed_basis()?,
        metadata()?,
        complete.acquisition_peak_bytes, // fresh scope/coverage while frozen state lives
        keys,
        query,
        selected.checked_add(candidates)?.max(evidence),
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
        assert!(metadata().unwrap() >= schema_map(23).unwrap());
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
}
