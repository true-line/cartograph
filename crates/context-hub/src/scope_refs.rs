use super::{
    BTreeSet, ContextError, ContextSnapshot, FactReference, MAX_NEIGHBORHOOD_HOPS, QueryScope,
    validate_limit,
};

impl ContextSnapshot {
    /// Select all references in an admitted scope without cloning properties or
    /// constructing fact response pages. The caller separately admits/retains
    /// this snapshot and the consumed scope. No kind or label filter is applied.
    pub fn references_in_scope(
        &self,
        scope: QueryScope,
    ) -> Result<BTreeSet<FactReference>, ContextError> {
        let neighborhood = match scope {
            QueryScope::All => None,
            QueryScope::Neighborhood { anchor, hops } => {
                validate_limit("hops", hops, MAX_NEIGHBORHOOD_HOPS)?;
                if !self.node_ids.contains(&anchor) {
                    return Err(ContextError::UnknownAnchor(anchor));
                }
                Some(self.neighborhood(&anchor, hops))
            }
        };
        Ok(self
            .facts
            .iter()
            .filter(|fact| contains(&fact.reference, neighborhood.as_ref()))
            .map(|fact| fact.reference.clone())
            .collect())
    }
}

/// Shared induced-membership rule; dangling edges remain visible in All only.
pub(super) fn contains(reference: &FactReference, nodes: Option<&BTreeSet<String>>) -> bool {
    nodes.is_none_or(|ids| match reference {
        FactReference::Node { id } => ids.contains(id),
        FactReference::Edge {
            source,
            destination,
            ..
        } => ids.contains(source) && ids.contains(destination),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MAX_RESPONSE_BYTES, QueryRequest, logical};
    use core_graph::{Edge, Node, SnapshotReadLimits};
    use serde_json::json;

    fn node(id: &str) -> Node {
        Node {
            id: id.into(),
            label: "Symbol".into(),
            props: json!({}),
        }
    }
    fn edge(src: &str, dst: &str) -> Edge {
        Edge {
            src: src.into(),
            label: "CALLS".into(),
            dst: dst.into(),
            props: json!({}),
        }
    }

    // AC-0328: exact membership parity includes induced and dangling edges.
    #[test]
    fn scope_references_match_query_pages_and_refuse_invalid_scope() {
        let snapshot = ContextSnapshot::new(
            vec![node("a"), node("b"), node("c")],
            vec![edge("a", "b"), edge("b", "c"), edge("a", "external")],
        )
        .unwrap();
        for scope in [
            QueryScope::All,
            QueryScope::Neighborhood {
                anchor: "a".into(),
                hops: 1,
            },
        ] {
            let mut request = QueryRequest {
                scope: scope.clone(),
                max_facts: 1,
                ..Default::default()
            };
            let mut paged = BTreeSet::new();
            loop {
                let page = snapshot.query(request.clone()).unwrap();
                paged.extend(page.facts.into_iter().map(|fact| fact.reference));
                request.cursor = page.next_cursor;
                if request.cursor.is_none() {
                    break;
                }
            }
            assert_eq!(snapshot.references_in_scope(scope).unwrap(), paged);
        }
        assert_eq!(
            snapshot
                .references_in_scope(QueryScope::Neighborhood {
                    anchor: "a".into(),
                    hops: 1
                })
                .unwrap(),
            BTreeSet::from([
                FactReference::Node { id: "a".into() },
                FactReference::Node { id: "b".into() },
                FactReference::Edge {
                    source: "a".into(),
                    label: "CALLS".into(),
                    destination: "b".into()
                },
            ])
        );
        for hops in [0, MAX_NEIGHBORHOOD_HOPS + 1] {
            assert!(matches!(
                snapshot.references_in_scope(QueryScope::Neighborhood {
                    anchor: "a".into(),
                    hops
                }),
                Err(ContextError::InvalidLimit { name: "hops", .. })
            ));
        }
        assert!(
            matches!(snapshot.references_in_scope(QueryScope::Neighborhood { anchor: "missing".into(), hops: 1 }),
            Err(ContextError::UnknownAnchor(anchor)) if anchor == "missing")
        );
    }

    // AC-0327/0328: references need no fact page, even for an oversized payload.
    #[test]
    fn scope_references_keep_oversized_facts_and_checked_workspace() {
        let mut large = node("large");
        large.props = json!({"text": "x".repeat(MAX_RESPONSE_BYTES + 1)});
        let snapshot = ContextSnapshot::new(vec![large], vec![]).unwrap();
        assert!(matches!(
            snapshot.query(QueryRequest::default()),
            Err(ContextError::ResponseBudgetExceeded { .. })
        ));
        assert_eq!(
            snapshot.references_in_scope(QueryScope::All).unwrap(),
            BTreeSet::from([FactReference::Node { id: "large".into() }])
        );
        let limits = SnapshotReadLimits::default();
        let model = logical::reference_selection_workspace(limits).unwrap();
        assert!(model.peak_bytes > model.references_bytes);
        for larger in [
            SnapshotReadLimits {
                max_bytes: limits.max_bytes + 1,
                ..limits
            },
            SnapshotReadLimits {
                max_rows: limits.max_rows + 1,
                ..limits
            },
        ] {
            let larger = logical::reference_selection_workspace(larger).unwrap();
            assert!(larger.references_bytes > model.references_bytes);
            assert!(larger.peak_bytes > model.peak_bytes);
        }
        for oversized in [
            SnapshotReadLimits {
                max_bytes: usize::MAX,
                ..limits
            },
            SnapshotReadLimits {
                max_rows: usize::MAX,
                ..limits
            },
        ] {
            assert_eq!(logical::reference_selection_workspace(oversized), None);
        }
    }
}
