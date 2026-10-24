use super::*;

/// Logical owners for a core query, excluding its borrowed snapshot and any
/// coverage wrapper. Not allocator capacity, native memory or process RSS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryWorkspace {
    /// Response facts and cursor, including a fact cloned before byte refusal.
    pub response_bytes: usize,
    /// Request/selection and temporary owners, including the response above.
    pub peak_bytes: usize,
}

/// The caller must separately admit both the snapshot and the serialized
/// request: query's public limits do not cap label, anchor or cursor strings.
/// `limits` bounds the source projection, not the successful response size.
/// Arithmetic overflow refuses the component. Framing and coverage envelope
/// ownership must be composed separately by the eventual worker admission.
pub fn query_workspace(limits: SnapshotReadLimits, request_wire: usize) -> Option<QueryWorkspace> {
    let request = sum(&[
        json_bytes(request_wire)?,
        set::<String>(request_wire)?, // normalized labels; payload moves
        size_of::<crate::QueryRequest>(),
    ])?;
    let selection_id = "selection-v1:".len().checked_add(blake3::OUT_LEN * 2)?;
    let snapshot_id = "context-v1:".len().checked_add(blake3::OUT_LEN * 2)?;
    // Use the full projection currency conservatively, including its indexes.
    // This does not claim that query actually constructs another snapshot.
    let response_bytes = sum(&[
        snapshot(limits)?.retained,
        size_of::<crate::QueryResponse>(),
        snapshot_id.checked_mul(2)?,
        selection_id,
    ])?;
    let identity = sum(&[
        json_bytes(request_wire)?,
        map::<String, Value>(request_wire)?, // rebuilt canonical object containers
        slots::<Value>(request_wire)?,       // rebuilt canonical arrays
        request_wire,
        count(&("selection-v1", Value::Null))?,
        selection_id,
    ])?;
    let ids = sum(&[limits.max_bytes, set::<String>(limits.max_rows)?])?;
    let neighborhood = ids.checked_mul(3)?; // visited, frontier, next frontier
    let page = sum(&[
        slots::<&ContextFact>(limits.max_rows)?, // borrowed selected facts
        response_bytes,
        selection_id,                    // retained through page assembly
        size_of::<crate::QueryCursor>(), // replacement overlaps previous cursor
        snapshot_id,
        selection_id,
    ])?;
    // The visited neighborhood survives identity construction and page assembly.
    let peak_bytes = sum(&[request, neighborhood, identity.max(page)])?;
    Some(QueryWorkspace {
        response_bytes,
        peak_bytes,
    })
}

/// Compose the actual `CoverageContext::query` wrapper around the core query.
/// `request_wire` bounds the complete qualified request before this call; its
/// optional context ID has no independent public size limit. Coverage inputs
/// use the same separately admitted limits as `coverage`. This excludes the
/// borrowed frozen context, host evidence/guards, a previous page retained by
/// a caller during replacement, transport encoding and allocator/native/RSS.
/// The complete coverage retained currency conservatively bounds its clone;
/// this does not imply that query rebuilds inventories or the shared snapshot.
pub fn qualified_query_workspace(
    limits: SnapshotReadLimits,
    coverage_limits: CoverageReadLimits,
    scoped_repos: usize,
    scope_identity_bytes: usize,
    request_wire: usize,
) -> Option<QueryWorkspace> {
    let core = query_workspace(limits, request_wire)?;
    let coverage = coverage(coverage_limits, scoped_repos, scope_identity_bytes)?;
    let outer_request = sum(&[
        request_wire, // context ID survives inner query and response assembly
        size_of::<crate::coverage::QualifiedRequest<crate::QueryRequest>>(),
    ])?;
    let response_bytes = sum(&[
        core.response_bytes,
        coverage.retained,
        "context-v2:".len().checked_add(blake3::OUT_LEN * 2)?,
        size_of::<crate::coverage::QualifiedResponse<crate::QueryResponse>>(),
    ])?;
    // inner_budget counts a borrowed envelope without allocating a JSON copy.
    // The core query completes before respond clones the qualification.
    let peak_bytes = sum(&[outer_request, core.peak_bytes.max(response_bytes)])?;
    Some(QueryWorkspace {
        response_bytes,
        peak_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContextError, MAX_RESPONSE_BYTES, QueryRequest, QueryScope};

    // AC-0327: successful response bytes cannot bound the first cloned fact.
    #[test]
    fn query_workspace_covers_rejected_fact_and_input_growth() {
        let text = "x".repeat(MAX_RESPONSE_BYTES + 1);
        let snapshot = ContextSnapshot::new(
            vec![Node {
                id: "a".into(),
                label: "Rule".into(),
                props: serde_json::json!({"text": text}),
            }],
            vec![],
        )
        .unwrap();
        let request = QueryRequest {
            scope: QueryScope::Neighborhood {
                anchor: "a".into(),
                hops: 1,
            },
            labels: vec!["Rule".into()],
            max_bytes: 1,
            ..Default::default()
        };
        let limits = SnapshotReadLimits::default();
        let wire = count(&request).unwrap();
        let model = query_workspace(limits, wire).unwrap();
        assert!(matches!(snapshot.query(request),
            Err(ContextError::ResponseBudgetExceeded { required_bytes, .. })
                if required_bytes > MAX_RESPONSE_BYTES));
        assert!(model.response_bytes > MAX_RESPONSE_BYTES);
        assert!(model.peak_bytes > model.response_bytes);
        let longer = query_workspace(limits, wire + 1024).unwrap();
        assert_eq!(longer.response_bytes, model.response_bytes);
        assert!(longer.peak_bytes > model.peak_bytes);
        for larger in [
            SnapshotReadLimits {
                max_bytes: limits.max_bytes + 1024,
                ..limits
            },
            SnapshotReadLimits {
                max_rows: limits.max_rows + 1,
                ..limits
            },
            SnapshotReadLimits {
                max_values: limits.max_values + 1,
                ..limits
            },
        ] {
            let larger = query_workspace(larger, wire).unwrap();
            assert!(larger.response_bytes > model.response_bytes);
            assert!(larger.peak_bytes > model.peak_bytes);
        }
    }

    // AC-0327: no saturating arithmetic or silent admission on overflow.
    #[test]
    fn query_workspace_rejects_unrepresentable_components() {
        let limits = SnapshotReadLimits::default();
        assert_eq!(query_workspace(limits, usize::MAX), None);
        for oversized in [
            SnapshotReadLimits {
                max_bytes: usize::MAX,
                ..limits
            },
            SnapshotReadLimits {
                max_rows: usize::MAX,
                ..limits
            },
            SnapshotReadLimits {
                max_values: usize::MAX,
                ..limits
            },
        ] {
            assert_eq!(query_workspace(oversized, 128), None);
        }
    }
    // AC-0327: qualification growth and all independent inputs fail checked.
    #[test]
    fn qualified_query_workspace_composes_and_refuses_overflow() {
        let graph = SnapshotReadLimits::default();
        let cov = CoverageReadLimits::default();
        let base = qualified_query_workspace(graph, cov, 1, 64, 128).unwrap();
        let core = query_workspace(graph, 128).unwrap();
        assert!(base.response_bytes > core.response_bytes);
        assert!(base.peak_bytes > core.peak_bytes);
        for (repos, identities, request) in [(2, 64, 128), (1, 128, 128), (1, 64, 256)] {
            let larger = qualified_query_workspace(graph, cov, repos, identities, request).unwrap();
            assert!(larger.peak_bytes > base.peak_bytes);
        }
        for (repos, identities, request) in [
            (usize::MAX, 64, 128),
            (1, usize::MAX, 128),
            (1, 64, usize::MAX),
        ] {
            assert_eq!(
                qualified_query_workspace(graph, cov, repos, identities, request),
                None
            );
        }
    }
}
