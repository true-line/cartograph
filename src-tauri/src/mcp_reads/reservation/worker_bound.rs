//! Composition of module-owned logical phase bounds. No allocator/RSS claim.
use super::{evidence, session};
use crate::mcp_reads::{dispatch, worker::WorkerLimits};
use context_hub::logical::{json_bytes, slots, sum};

#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkerReservation {
    pub required_bytes: usize,
    #[cfg(test)]
    pub capability_compile_bytes: usize,
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
            artifact_basis,
            evidence,
            discovery,
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
