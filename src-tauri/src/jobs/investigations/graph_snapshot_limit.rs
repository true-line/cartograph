//! Closed `host_limit` diagnostics for the graph snapshot reads of scoped
//! preparation (#1602; ADR-0052 amendment 2026-11-18). The message names the
//! preparation phase and the limit that was hit, never a fact or a number.

/// Which preparation read hit its limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphSnapshotPhase {
    /// The streamed complete-graph identity pass (ADR-0052 ceilings in
    /// aggregate, the retained limits for the one row it holds).
    Identity,
    /// The scoped facts that preparation keeps (the task's retained limits).
    Scope,
}

/// Which snapshot limit was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphSnapshotLimitKind {
    /// Combined node and edge rows.
    Rows,
    /// Raw stored or canonical graph bytes.
    Bytes,
    /// Cumulative JSON values.
    Values,
    /// JSON depth.
    Depth,
    /// One identity-pass row's raw bytes, over the retained-row cap.
    RowBytes,
    /// One identity-pass row's property values, over the retained-row cap.
    RowValues,
}

/// A preparation snapshot read that stopped at a named limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphSnapshotLimit {
    pub phase: GraphSnapshotPhase,
    pub limit: GraphSnapshotLimitKind,
}

impl GraphSnapshotLimit {
    /// The named limit behind `error`, or `None` when it is not a snapshot
    /// size limit (invalid input, storage or coverage failures keep their
    /// existing classification).
    pub fn from_graph_error(
        phase: GraphSnapshotPhase,
        error: &core_graph::GraphError,
    ) -> Option<Self> {
        use GraphSnapshotLimitKind as Kind;
        use core_graph::SnapshotBoundsError as Bounds;
        let core_graph::GraphError::SnapshotBounds(bounds) = error else {
            return None;
        };
        let limit = match bounds {
            Bounds::RowLimit => Kind::Rows,
            Bounds::RawByteLimit | Bounds::CanonicalByteLimit => Kind::Bytes,
            Bounds::JsonValueLimit => Kind::Values,
            Bounds::JsonDepthLimit => Kind::Depth,
            Bounds::SingleRowByteLimit => Kind::RowBytes,
            Bounds::SingleRowValueLimit => Kind::RowValues,
            _ => return None,
        };
        Some(Self { phase, limit })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn all() -> impl Iterator<Item = Self> {
        use GraphSnapshotLimitKind as Kind;
        [GraphSnapshotPhase::Identity, GraphSnapshotPhase::Scope]
            .into_iter()
            .flat_map(|phase| {
                [
                    Kind::Rows,
                    Kind::Bytes,
                    Kind::Values,
                    Kind::Depth,
                    Kind::RowBytes,
                    Kind::RowValues,
                ]
                .into_iter()
                .map(move |limit| Self { phase, limit })
            })
    }

    pub fn message(self) -> &'static str {
        use GraphSnapshotLimitKind as Kind;
        use GraphSnapshotPhase as Phase;
        macro_rules! message {
            ($phase:literal, $limit:literal) => {
                concat!(
                    "Investigation limit reached: host_limit; phase=",
                    $phase,
                    "; limit=",
                    $limit,
                    "."
                )
            };
        }
        match (self.phase, self.limit) {
            (Phase::Identity, Kind::Rows) => message!("graph_identity", "snapshot_rows"),
            (Phase::Identity, Kind::Bytes) => message!("graph_identity", "snapshot_bytes"),
            (Phase::Identity, Kind::Values) => message!("graph_identity", "snapshot_values"),
            (Phase::Identity, Kind::Depth) => message!("graph_identity", "snapshot_depth"),
            (Phase::Identity, Kind::RowBytes) => {
                message!("graph_identity", "snapshot_row_bytes")
            }
            (Phase::Identity, Kind::RowValues) => {
                message!("graph_identity", "snapshot_row_values")
            }
            (Phase::Scope, Kind::Rows) => message!("graph_scope", "snapshot_rows"),
            (Phase::Scope, Kind::Bytes) => message!("graph_scope", "snapshot_bytes"),
            (Phase::Scope, Kind::Values) => message!("graph_scope", "snapshot_values"),
            (Phase::Scope, Kind::Depth) => message!("graph_scope", "snapshot_depth"),
            (Phase::Scope, Kind::RowBytes) => message!("graph_scope", "snapshot_row_bytes"),
            (Phase::Scope, Kind::RowValues) => message!("graph_scope", "snapshot_row_values"),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;
    use crate::jobs::investigations::InvestigationStopReason as Stop;
    use core_graph::{GraphError, SnapshotBoundsError as Bounds};

    // T-0776 (#1602): a preparation snapshot limit names its phase and the
    // limit hit, is a limit (not a preparation or operational) stop, and is
    // recognized as a host_limit message by the acceptance records.
    #[test]
    fn graph_snapshot_limit_names_phase_and_limit() {
        let identity = |error: Bounds| {
            GraphSnapshotLimit::from_graph_error(
                GraphSnapshotPhase::Identity,
                &GraphError::SnapshotBounds(error),
            )
            .map(|limit| Stop::GraphSnapshotLimit(limit).test_message())
        };
        assert_eq!(
            identity(Bounds::RowLimit),
            Some(
                "Investigation limit reached: host_limit; phase=graph_identity; limit=snapshot_rows."
            )
        );
        assert_eq!(
            identity(Bounds::RawByteLimit),
            Some(
                "Investigation limit reached: host_limit; phase=graph_identity; limit=snapshot_bytes."
            )
        );
        assert_eq!(
            identity(Bounds::CanonicalByteLimit),
            identity(Bounds::RawByteLimit)
        );
        assert_eq!(
            identity(Bounds::SingleRowByteLimit),
            Some(
                "Investigation limit reached: host_limit; phase=graph_identity; limit=snapshot_row_bytes."
            )
        );
        assert_eq!(
            GraphSnapshotLimit::from_graph_error(
                GraphSnapshotPhase::Scope,
                &GraphError::SnapshotBounds(Bounds::RowLimit)
            )
            .map(|limit| Stop::GraphSnapshotLimit(limit).test_message()),
            Some(
                "Investigation limit reached: host_limit; phase=graph_scope; limit=snapshot_rows."
            )
        );
        // Not a size limit: the existing classification stands.
        for error in [
            Bounds::InvalidLimits,
            Bounds::InvalidRow,
            Bounds::InvalidJson,
            Bounds::InvalidSchema,
            Bounds::CoverageRecordLimit,
            Bounds::CoverageByteLimit,
        ] {
            assert_eq!(identity(error), None, "{error:?}");
        }
        let mut messages = std::collections::BTreeSet::new();
        for limit in GraphSnapshotLimit::all() {
            let stop = Stop::GraphSnapshotLimit(limit);
            assert_eq!(stop.terminal_class(), Stop::LimitExceeded);
            assert!(Stop::is_host_limit_message(stop.test_message()));
            assert!(messages.insert(stop.test_message()));
        }
        assert_eq!(messages.len(), 12);
        assert!(!Stop::is_host_limit_message(
            "Investigation limit reached: host_limit; phase=graph_identity; limit=untrusted."
        ));
    }
}
