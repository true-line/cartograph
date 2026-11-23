use super::*;
use crate::jobs::investigations::{InvestigationDetailRead, ScopeSize};

fn size(scope_snapshot_id: &str, nodes: u64, edges: u64) -> ScopeSize {
    ScopeSize {
        scope_snapshot_id: scope_snapshot_id.into(),
        nodes,
        edges,
    }
}

// #2475: a prepared task names its scope's node and edge counts, bound to its
// saved scope snapshot. The first record stands, reopen reads it back, and a
// task with no record (a paged whole-system scope) names none.
#[test]
fn a_prepared_scope_names_its_size_and_reopen_keeps_it() {
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("scope-size", InvestigationProviderMode::Local);
    let (other, _other_execution) =
        fixture.start("scope-size-none", InvestigationProviderMode::Local);
    // Before preparation there is no scope snapshot to bind a size to.
    assert!(matches!(
        fixture
            .store
            .record_investigation_scope_size(&id, &size("scope:any", 1, 0)),
        Err(InvestigationStoreError::Invalid)
    ));
    let ledger = fixture.prepare(&id, &execution, false);
    let scope = ledger.scope_snapshot_id.as_str();
    assert_eq!(fixture.store.investigation_scope_size(&id).unwrap(), None);

    fixture
        .store
        .record_investigation_scope_size(&id, &size(scope, 312, 540))
        .unwrap();
    // A repeat of the same size is not a new record.
    fixture
        .store
        .record_investigation_scope_size(&id, &size(scope, 312, 540))
        .unwrap();
    for invalid in [
        size(scope, 313, 540),
        size("scope:other", 312, 540),
        size("", 1, 1),
        size(scope, 1 << 41, 0),
    ] {
        assert!(matches!(
            fixture.store.record_investigation_scope_size(&id, &invalid),
            Err(InvestigationStoreError::Invalid)
        ));
    }
    assert!(matches!(
        fixture
            .store
            .record_investigation_scope_size("inv_missing", &size(scope, 1, 1)),
        Err(InvestigationStoreError::Missing | InvestigationStoreError::Invalid)
    ));

    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_scope_size(&id).unwrap(),
        Some(size(scope, 312, 540))
    );
    assert_eq!(reopened.investigation_scope_size(&other).unwrap(), None);

    let detail = reopened.investigation(&id).unwrap();
    let read = InvestigationDetailRead::from_detail(detail.clone())
        .unwrap()
        .with_scope_size(reopened.investigation_scope_size(&id).unwrap());
    let json = serde_json::to_value(&read).unwrap();
    assert_eq!(
        json["scope_size"],
        serde_json::json!({"scope_snapshot_id": scope, "nodes": 312, "edges": 540})
    );
    assert_eq!(json["scope_snapshot_id"], scope);
    let plain =
        serde_json::to_value(InvestigationDetailRead::from_detail(detail).unwrap()).unwrap();
    assert!(plain.get("scope_size").is_none());

    // The saved row is immutable.
    let tamper = reopened
        .conn
        .execute("UPDATE scope_size_records SET nodes=1", []);
    assert!(tamper.is_err());
}
