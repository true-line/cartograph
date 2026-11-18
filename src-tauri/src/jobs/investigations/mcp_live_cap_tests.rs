use super::*;

// #1761 / ADR-0073 question 4: the principal identity cap is a local
// live-work ceiling, not a lifetime quota.
fn identity(store: &mut JobStore, maximum_identities: usize) -> McpRequestIdentity {
    McpRequestIdentity {
        client: store
            .create_mcp_client(std::num::NonZeroUsize::new(2).unwrap())
            .unwrap(),
        normalized_intent_hash: core_prov::content_hash(b"normalized caller intent"),
        maximum_identities,
    }
}

fn start(
    store: &mut JobStore,
    nonce: &str,
    identity: &McpRequestIdentity,
) -> Result<InvestigationStart, InvestigationStoreError> {
    store.start_investigation_internal(
        &request(nonce, InvestigationProviderMode::Local)
            .normalized()
            .unwrap(),
        Some(&provider(InvestigationProviderMode::Local)),
        None,
        false,
        Some(identity),
    )
}

fn stored_rows(store: &JobStore) -> i64 {
    store
        .conn
        .query_row("SELECT count(*) FROM investigation_tasks", [], |r| r.get(0))
        .unwrap()
}

// AC-0840: terminal tasks the principal owns do not count toward its cap.
#[test]
fn mcp_principal_cap_ignores_terminal_tasks() {
    let mut f = Fixture::new();
    let identity = identity(&mut f.store, 1);
    for nonce in ["first", "second", "third"] {
        let started = start(&mut f.store, nonce, &identity).unwrap();
        assert!(started.created);
        let cancelled = f
            .store
            .cancel_investigation(&started.detail.summary.investigation_id)
            .unwrap();
        assert_eq!(cancelled.summary.status, InvestigationStatus::Cancelled);
    }
    // Three stored rows, none live: a cap of one still admits new work.
    assert_eq!(stored_rows(&f.store), 3);
    assert!(start(&mut f.store, "fourth", &identity).unwrap().created);
    assert_eq!(stored_rows(&f.store), 4);
}

// AC-0841: live tasks still reach the cap, refused before any write, while the
// global live ceiling has room (so the refusal is the principal's).
#[test]
fn mcp_principal_cap_counts_live_tasks() {
    let mut f = Fixture::new();
    let identity = identity(&mut f.store, 1);
    let finished = start(&mut f.store, "finished", &identity).unwrap();
    f.store
        .cancel_investigation(&finished.detail.summary.investigation_id)
        .unwrap();
    let live = start(&mut f.store, "live", &identity).unwrap();
    assert!(live.created);
    let changes = f.store.conn.total_changes();
    assert_eq!(
        start(&mut f.store, "over-cap", &identity).unwrap_err(),
        InvestigationStoreError::Capacity
    );
    assert_eq!(f.store.conn.total_changes(), changes);
    // One global live slot remained free throughout.
    assert!(
        f.store
            .start_investigation(
                &request("app-task", InvestigationProviderMode::Local),
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
            .created
    );
    // Finishing the live task frees the principal's slot again.
    f.store
        .cancel_investigation(&live.detail.summary.investigation_id)
        .unwrap();
    assert!(start(&mut f.store, "over-cap", &identity).unwrap().created);
}

// AC-0842: the same principal, nonce and intent returns the existing task,
// terminal or not, even while the principal is at its live cap.
#[test]
fn mcp_principal_cap_retry_returns_terminal_task() {
    let mut f = Fixture::new();
    let mut identity = identity(&mut f.store, 1);
    let first = start(&mut f.store, "retry", &identity).unwrap();
    let terminal = f
        .store
        .cancel_investigation(&first.detail.summary.investigation_id)
        .unwrap();
    assert!(start(&mut f.store, "occupant", &identity).unwrap().created);
    let rows = stored_rows(&f.store);
    let replay = start(&mut f.store, "retry", &identity).unwrap();
    assert!(!replay.created);
    assert_eq!(replay.detail, terminal);
    assert_eq!(replay.detail.summary.status, InvestigationStatus::Cancelled);
    assert_eq!(stored_rows(&f.store), rows);
    identity.normalized_intent_hash = core_prov::content_hash(b"different caller intent");
    assert_eq!(
        start(&mut f.store, "retry", &identity).unwrap_err(),
        InvestigationStoreError::Conflict
    );
}
