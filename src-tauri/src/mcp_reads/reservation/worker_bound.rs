//! Composition of module-owned logical phase bounds. No allocator/RSS claim.
use super::{evidence, session};
use crate::mcp_reads::{dispatch, worker::WorkerLimits};
use context_hub::logical::{json_bytes, slots, sum};

#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkerReservation {
    pub required_bytes: usize,
    #[cfg(test)]
    pub capability_compile_bytes: usize,
    #[cfg(test)]
    pub coverage_compile_bytes: usize,
}
impl WorkerReservation {
    pub(crate) fn for_limits(limits: WorkerLimits) -> Option<Self> {
        let session = session::SessionReservation::for_limits(limits)?;
        let graph = flowtracer::TraceGraphBound {
            bytes: limits.graph.max_bytes,
            rows: limits.graph.max_rows,
            values: limits.graph.max_values,
        };
        let trace = flowtracer::logical_workspace_bound(graph, limits.dispatch.trace)?;
        let compile =
            spec::logical_compile_bound(graph, limits.dispatch.trace, limits.dispatch.compile)?;
        let (n, bytes) = crate::mcp_reads::capability_caps(limits.graph);
        let knowledge = spec::logical_knowledge_story_compile_bound(
            graph,
            limits.dispatch.trace,
            limits.dispatch.compile,
            n,
            bytes,
        )?;
        // Borrowed candidate input belongs to the host, not the compiler helper.
        // It stays through compilation and drops before basis_id.
        let capability_compile_bytes = sum(&[
            knowledge.peak_bytes,
            slots::<spec::CapabilityStory<'_>>(n)?,
            std::mem::size_of::<Vec<spec::CapabilityStory<'_>>>(),
        ])?;
        // The coverage artifacts (AC-0657) compile through the same selected
        // compiler, which projects the graph whatever is selected. The
        // inventories are the session's retained rows, borrowed; the
        // producers' own workspace and the host's stale-repository rows (at
        // most one per qualified repository) are added here, since
        // `logical_compile_bound` models neither.
        let coverage_compile_bytes = sum(&[
            compile.peak_bytes,
            spec::logical_coverage_render_bound(
                limits.coverage.max_records,
                limits.coverage.max_bytes,
            )?,
            slots::<spec::StaleCoverage<'_>>(
                limits
                    .coverage
                    .max_records
                    .checked_add(limits.scope.max_sources.get())?,
            )?,
            std::mem::size_of::<Vec<spec::StaleCoverage<'_>>>(),
        ])?;
        // basis_id is computed after selected compilation, with artifact and
        // original host flows still retained, but compiler projection scratch gone.
        let artifact_basis = sum(&[
            compile.original_flows_bytes,
            compile.artifact_bytes,
            session.basis_digest_bytes,
        ])?;
        let freshness = session::freshness_snapshot_peak_bytes(limits.graph)?;
        let evidence = evidence::evidence_peak_bytes(freshness, limits.evidence)?;
        let discovery = session::discovery_peak_bytes(limits)?;
        let query = session::query_peak_bytes(limits)?;
        let protocol = dispatch::logical_protocol_workspace_bytes(limits)?;
        let read = limits.max_request_frame_bytes.get();
        let (ids, current_id) =
            session::request_ids_bytes(limits.dispatch.max_request_ids.get(), read)?;
        // Beyond the transport's parsed message/argument clone, handlers own a
        // typed decoded argument, its canonical query/target representation and
        // the serialized identity scratch. Each is bounded by input frame bytes
        // in the uniform JSON/header currency, including escaped diagnostics.
        let current_argument = sum(&[json_bytes(read)?, json_bytes(read)?, read, current_id])?;
        let request_peak = [
            trace.peak_bytes,
            compile.peak_bytes,
            capability_compile_bytes,
            coverage_compile_bytes,
            artifact_basis,
            evidence,
            discovery,
            query,
            protocol,
        ]
        .into_iter()
        .max()?;
        let ready_peak = sum(&[
            session.retained_session_bytes,
            ids,
            current_argument,
            request_peak,
        ])?;
        Some(Self {
            #[cfg(test)]
            capability_compile_bytes,
            #[cfg(test)]
            coverage_compile_bytes,
            // Lazy acquisition runs after protocol admission, with prior IDs and
            // the admitted call still alive.
            required_bytes: sum(&[session.acquisition_peak_bytes, ids, current_argument])?
                .max(ready_peak),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroUsize;

    #[test]
    fn worker_floor_covers_acquisition_and_retained_request_phases() {
        // AC-0288: reservations are derived before acquiring a worker permit.
        let limits = crate::mcp_reads::worker::tests::limits();
        let bound = WorkerReservation::for_limits(limits).unwrap();
        let session = session::SessionReservation::for_limits(limits).unwrap();
        let read = limits.max_request_frame_bytes.get();
        let (ids, current_id) =
            session::request_ids_bytes(limits.dispatch.max_request_ids.get(), read).unwrap();
        let argument = sum(&[
            json_bytes(read).unwrap(),
            json_bytes(read).unwrap(),
            read,
            current_id,
        ])
        .unwrap();
        assert!(
            bound.required_bytes >= sum(&[session.acquisition_peak_bytes, ids, argument]).unwrap()
        );
        assert!(bound.required_bytes > session.retained_session_bytes);
        let mut larger = limits;
        larger.max_request_frame_bytes =
            NonZeroUsize::new(limits.max_request_frame_bytes.get().checked_mul(2).unwrap())
                .unwrap();
        assert!(
            WorkerReservation::for_limits(larger)
                .unwrap()
                .required_bytes
                >= bound.required_bytes
        );
        // The offered reservation is not input to the derived minimum.
        larger = limits;
        larger.reserved_bytes = NonZeroUsize::new(1).unwrap();
        assert_eq!(
            WorkerReservation::for_limits(larger)
                .unwrap()
                .required_bytes,
            bound.required_bytes
        );
    }

    // AC-0657 (#1814): the supplied coverage inventories are a modelled
    // compile input. The coverage compile is the ordinary selected compile
    // plus the producers' workspace over the admitted inventories, it is
    // inside the worker floor beside the retained session, and it grows
    // with either coverage limit. The retained session keeps the stored rows.
    #[test]
    fn worker_floor_covers_the_coverage_compile_over_retained_inventories() {
        let limits = crate::mcp_reads::worker::tests::limits();
        let bound = WorkerReservation::for_limits(limits).unwrap();
        let session = session::SessionReservation::for_limits(limits).unwrap();
        let graph = flowtracer::TraceGraphBound {
            bytes: limits.graph.max_bytes,
            rows: limits.graph.max_rows,
            values: limits.graph.max_values,
        };
        let compile =
            spec::logical_compile_bound(graph, limits.dispatch.trace, limits.dispatch.compile)
                .unwrap();
        let render = spec::logical_coverage_render_bound(
            limits.coverage.max_records,
            limits.coverage.max_bytes,
        )
        .unwrap();
        assert!(render >= limits.coverage.max_bytes);
        assert!(bound.coverage_compile_bytes >= compile.peak_bytes + render);
        assert!(
            bound.required_bytes >= session.retained_session_bytes + bound.coverage_compile_bytes
        );
        let stored = context_hub::logical::coverage(
            limits.coverage,
            limits.scope.max_sources.get(),
            limits.scope.max_source_registry_bytes.get(),
        )
        .unwrap();
        assert!(session.retained_session_bytes >= stored.retained + stored.stored);
        let edits: &[fn(&mut WorkerLimits)] = &[
            |limits| limits.coverage.max_bytes += 1,
            |limits| limits.coverage.max_records += 1,
        ];
        for edit in edits {
            let mut larger = limits;
            edit(&mut larger);
            let larger = WorkerReservation::for_limits(larger).unwrap();
            assert!(larger.coverage_compile_bytes > bound.coverage_compile_bytes);
            assert!(larger.required_bytes > bound.required_bytes);
        }
        let mut overflow = limits;
        overflow.coverage.max_bytes = usize::MAX;
        assert!(WorkerReservation::for_limits(overflow).is_none());
    }

    #[test]
    fn worker_floor_rejects_unrepresentable_request_roles() {
        let mut limits = crate::mcp_reads::worker::tests::limits();
        limits.max_request_frame_bytes = NonZeroUsize::new(usize::MAX).unwrap();
        assert!(WorkerReservation::for_limits(limits).is_none());
        limits = crate::mcp_reads::worker::tests::limits();
        limits.dispatch.max_request_ids = NonZeroUsize::new(usize::MAX).unwrap();
        assert!(WorkerReservation::for_limits(limits).is_none());
    }
}
