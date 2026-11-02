use super::*;
use crate::investigation_basis::InvestigationScopeObservation;
use core_graph::GraphStore;
use serde_json::json;

fn store() -> SqliteGraphStore {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    for id in ["a", "b", "c"] {
        store
            .put_node(&Node {
                id: id.into(),
                label: "Symbol".into(),
                props: json!({}),
            })
            .unwrap();
    }
    store
        .put_edge(&Edge {
            src: "a".into(),
            dst: "b".into(),
            label: "CALLS".into(),
            props: json!({}),
        })
        .unwrap();
    store
}
fn traversal(
    store: &SqliteGraphStore,
    aggregate: SnapshotReadLimits,
) -> (InvestigationAllTraversal, String) {
    let basis = InvestigationReadBasis::new(
        store.read_revision().unwrap(),
        InvestigationScopeObservation {
            extent: InvestigationScopeExtent::AllPaged,
            request_digest: [1; 32],
            projection_digest: None,
            qualification_digest: [2; 32],
        },
    )
    .unwrap();
    let id = basis.id().to_owned();
    (
        InvestigationAllTraversal::new(
            basis,
            SnapshotReadLimits {
                max_rows: 1,
                ..Default::default()
            },
            aggregate,
        )
        .unwrap(),
        id,
    )
}
// AC-0304: node exhaustion alone never completes All; edge phase remains bounded.
#[test]
fn investigation_all_pages_visit_both_phases_without_whole_graph_claim() {
    let store = store();
    let (mut scan, id) = traversal(&store, SnapshotReadLimits::default());
    for expected in ["a", "b", "c"] {
        let page = scan.next(&store, &id).unwrap().unwrap();
        assert_eq!(page.nodes[0].id, expected);
        assert!(page.edges.is_empty());
        assert!(!page.traversal_complete);
    }
    let page = scan.next(&store, &id).unwrap().unwrap();
    assert!(page.nodes.is_empty());
    assert_eq!(page.edges.len(), 1);
    assert!(page.traversal_complete);
    assert!(scan.next(&store, &id).unwrap().is_none());
}
// AC-0304: cumulative structure budget is not reset at each page; failures are terminal.
#[test]
fn investigation_all_pages_charge_cumulative_values_and_poison_on_failure() {
    let store = store();
    let one = store
        .read_node_window(&NodeWindowRequest {
            expected_revision: None,
            after_id: None,
            limits: SnapshotReadLimits {
                max_rows: 1,
                ..Default::default()
            },
        })
        .unwrap();
    let (mut scan, id) = traversal(
        &store,
        SnapshotReadLimits {
            max_values: one.json_values,
            ..Default::default()
        },
    );
    scan.next(&store, &id).unwrap().unwrap();
    assert!(scan.next(&store, &id).is_err());
    assert!(matches!(
        scan.next(&store, &id),
        Err(InvestigationPageError::Failed)
    ));
}
// AC-0304: a changed revision or scope/qualification identity refuses continuation.
#[test]
fn investigation_all_pages_refuse_revision_or_basis_change() {
    let mut store = store();
    let (mut scan, id) = traversal(&store, SnapshotReadLimits::default());
    scan.next(&store, &id).unwrap();
    store
        .put_node(&Node {
            id: "d".into(),
            label: "Symbol".into(),
            props: json!({}),
        })
        .unwrap();
    assert!(matches!(
        scan.next(&store, &id),
        Err(InvestigationPageError::Storage(_))
    ));
    let (mut scan, _) = traversal(&store, SnapshotReadLimits::default());
    assert!(matches!(
        scan.next(&store, "changed-query-or-policy"),
        Err(InvestigationPageError::Basis(_))
    ));
}

// AC-0304: later pages cannot reset aggregate row or byte ceilings.
#[test]
fn investigation_all_pages_charge_cumulative_rows_and_bytes() {
    let store = store();
    let first = store
        .read_node_window(&NodeWindowRequest {
            expected_revision: None,
            after_id: None,
            limits: SnapshotReadLimits {
                max_rows: 1,
                ..Default::default()
            },
        })
        .unwrap();
    for aggregate in [
        SnapshotReadLimits {
            max_rows: 1,
            ..Default::default()
        },
        SnapshotReadLimits {
            max_bytes: first.raw_bytes.max(first.canonical_bytes),
            ..Default::default()
        },
    ] {
        let (mut scan, id) = traversal(&store, aggregate);
        assert!(!scan.next(&store, &id).unwrap().unwrap().traversal_complete);
        assert!(scan.next(&store, &id).is_err());
        assert!(matches!(
            scan.next(&store, &id),
            Err(InvestigationPageError::Failed)
        ));
    }
}

// AC-0304: an empty final phase consumes no additional row/byte/value allowance.
#[test]
fn investigation_all_pages_finish_empty_edges_at_exact_aggregate_limits() {
    for limit in 0..3 {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        store
            .put_node(&Node {
                id: "a".into(),
                label: "Symbol".into(),
                props: json!({}),
            })
            .unwrap();
        let one = store
            .read_node_window(&NodeWindowRequest {
                expected_revision: None,
                after_id: None,
                limits: Default::default(),
            })
            .unwrap();
        let mut aggregate = SnapshotReadLimits::default();
        match limit {
            0 => aggregate.max_rows = 1,
            1 => aggregate.max_bytes = one.raw_bytes.max(one.canonical_bytes),
            _ => aggregate.max_values = one.json_values,
        }
        let (mut scan, id) = traversal(&store, aggregate);
        assert!(!scan.next(&store, &id).unwrap().unwrap().traversal_complete);
        let final_page = scan.next(&store, &id).unwrap().unwrap();
        assert!(final_page.nodes.is_empty() && final_page.edges.is_empty());
        assert!(final_page.traversal_complete);
        assert!(scan.next(&store, &id).unwrap().is_none());

        store
            .put_edge(&Edge {
                src: "a".into(),
                dst: "a".into(),
                label: "CALLS".into(),
                props: json!({}),
            })
            .unwrap();
        let (mut scan, id) = traversal(&store, aggregate);
        scan.next(&store, &id).unwrap();
        assert!(scan.next(&store, &id).is_err());
        assert!(matches!(
            scan.next(&store, &id),
            Err(InvestigationPageError::Failed)
        ));
    }
}

// AC-0304: the no-payload exhaustion probe still observes the frozen revision.
#[test]
fn investigation_empty_edge_probe_refuses_changed_revision() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store
        .put_node(&Node {
            id: "a".into(),
            label: "Symbol".into(),
            props: json!({}),
        })
        .unwrap();
    let (mut scan, id) = traversal(
        &store,
        SnapshotReadLimits {
            max_rows: 1,
            ..Default::default()
        },
    );
    scan.next(&store, &id).unwrap();
    store
        .put_node(&Node {
            id: "b".into(),
            label: "Symbol".into(),
            props: json!({}),
        })
        .unwrap();
    assert!(scan.next(&store, &id).is_err());
}
