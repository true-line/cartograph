use super::*;

// Synthetic attribution only: no MCP creation or authentication is exposed.
fn attribute(store: &JobStore, id: &str, client: &McpClientId) {
    let bytes: Vec<u8> = store
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    let mut task: StoredTask = serde_json::from_slice(&bytes).unwrap();
    task.detail.origin = InvestigationOrigin::Mcp {
        client_id: client.clone(),
    };
    task.mcp_intent_hash = Some("a".repeat(64));
    let bytes = encode(&task, MAX_RECORD_BYTES).unwrap();
    store.conn.execute(
        "UPDATE investigation_tasks SET record=?2,record_hash=?3,request_namespace=?4 WHERE id=?1",
        params![id, bytes, core_prov::content_hash(&bytes), task.detail.origin.request_namespace()],
    ).unwrap();
}

fn denied(store: &mut JobStore, id: &str, client: &McpClientId) {
    let activation =
        McpTaskActivation::granted(client.clone(), std::num::NonZeroUsize::new(4).unwrap());
    denied_session(store, id, &activation.session());
}

fn denied_session(store: &mut JobStore, id: &str, session: &McpTaskSession) {
    let changes = store.conn.total_changes();
    assert_eq!(
        store.investigation_access(id, Some(session)).unwrap_err(),
        InvestigationStoreError::Missing
    );
    assert_eq!(
        store
            .investigation_result_access(id, Some(session))
            .unwrap_err(),
        InvestigationStoreError::Missing
    );
    // Ownership also precedes validation of the event cursor and family routing.
    assert_eq!(
        store
            .investigation_events_access(id, u64::MAX, Some(session))
            .unwrap_err(),
        InvestigationStoreError::Missing
    );
    for next in [false, true] {
        assert_eq!(
            store
                .cancel_investigation_access(id, next, Some(session))
                .unwrap_err(),
            InvestigationStoreError::Missing
        );
    }
    assert_eq!(store.conn.total_changes(), changes);
}

// AC-0324: indexed ownership is checked before loading any task content.
#[test]
fn principal_task_access_isolates_reads_cancel_and_corrupt_foreign_bodies() {
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("principal-owned", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, true);
    begin(&mut fixture, &id, &execution, step(&ledger, 1));
    let (app_id, _app_execution) = fixture.start("principal-app", InvestigationProviderMode::Local);
    let client = McpClientId::from_host_bytes([1; 16]);
    let other = McpClientId::from_host_bytes([2; 16]);
    attribute(&fixture.store, &id, &client);
    let activation =
        McpTaskActivation::granted(client.clone(), std::num::NonZeroUsize::new(4).unwrap());
    let session = activation.session();
    for (subject, principal) in [
        (id.as_str(), &other),
        (app_id.as_str(), &client),
        ("investigation:missing", &client),
    ] {
        denied(&mut fixture.store, subject, principal);
    }
    assert_eq!(
        fixture
            .store
            .investigation_access(&id, Some(&session))
            .unwrap(),
        fixture.store.investigation(&id).unwrap()
    );
    assert_eq!(
        fixture
            .store
            .investigation_result_access(&id, Some(&session))
            .unwrap(),
        None
    );
    assert_eq!(
        fixture
            .store
            .investigation_events_access(&id, 0, Some(&session))
            .unwrap(),
        fixture.store.investigation_events(&id, 0).unwrap()
    );
    let cancelled = fixture
        .store
        .cancel_investigation_access(&id, false, Some(&session))
        .unwrap();
    assert!(cancelled.summary.cancel_requested);
    assert_eq!(fixture.store.investigation(&id).unwrap(), cancelled);
    assert_eq!(fixture.store.cancel_investigation(&id).unwrap(), cancelled);
    assert!(
        !fixture
            .store
            .investigation(&app_id)
            .unwrap()
            .summary
            .cancel_requested
    );

    // A late result retains the existing cancellation/settlement semantics.
    let result = finish(&id, &ledger);
    fixture
        .store
        .advance_investigation(
            &execution,
            cancelled.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: response("step:1", 1),
                tool: None,
                result: Some(Box::new(result.clone())),
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .investigation_result_access(&id, Some(&session))
            .unwrap(),
        Some(result)
    );
    assert_eq!(
        fixture
            .store
            .investigation_result_access(&id, Some(&session))
            .unwrap(),
        fixture.store.investigation_result(&id).unwrap()
    );
    for (subject, principal) in [(app_id.as_str(), &client), (id.as_str(), &other)] {
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_tasks SET record=X'7b' WHERE id=?1",
                [subject],
            )
            .unwrap();
        denied(&mut fixture.store, subject, principal);
        assert_eq!(
            fixture.store.investigation(subject).unwrap_err(),
            InvestigationStoreError::Invalid
        );
    }
}

// AC-0320/0324: authority is checked for every operation, not a saved snapshot.
#[test]
fn granted_task_access_revocation_and_rotation_preserve_durable_owner_state() {
    let mut fixture = Fixture::new();
    let cap = std::num::NonZeroUsize::new(4).unwrap();
    let client = fixture.store.create_mcp_client(cap).unwrap();
    let identity = McpRequestIdentity {
        client: client.clone(),
        normalized_intent_hash: core_prov::content_hash(b"grant access"),
        maximum_identities: cap.get(),
    };
    let created = fixture
        .store
        .start_investigation_internal(
            &request("grant-owned", InvestigationProviderMode::Local)
                .normalized()
                .unwrap(),
            Some(&provider(InvestigationProviderMode::Local)),
            None,
            false,
            Some(&identity),
        )
        .unwrap();
    let id = created.detail.summary.investigation_id.clone();
    let read_only = McpTaskActivation::default();
    denied_session(&mut fixture.store, &id, &read_only.session());
    let activation = McpTaskActivation::granted(client.clone(), cap);
    let session = activation.session();
    assert_eq!(
        fixture
            .store
            .investigation_access(&id, Some(&session))
            .unwrap(),
        created.detail
    );
    assert_eq!(
        fixture
            .store
            .investigation_events_access(&id, 0, Some(&session))
            .unwrap(),
        fixture.store.investigation_events(&id, 0).unwrap()
    );
    assert!(
        fixture
            .store
            .investigation_result_access(&id, Some(&session))
            .unwrap()
            .is_none()
    );
    let observed = activation.owner_snapshot().unwrap().unwrap();
    let successor = activation.read_only_successor().unwrap();
    denied_session(&mut fixture.store, &id, &session);
    denied_session(&mut fixture.store, &id, &successor.session());
    assert_eq!(fixture.store.investigation(&id).unwrap(), created.detail);
    let renewed = McpTaskActivation::granted(observed.client_id, observed.maximum_identities);
    let renewed_session = renewed.session();
    // The wrong caller-supplied family hint cannot redirect MCP cancellation.
    let cancelled = fixture
        .store
        .cancel_investigation_access(&id, true, Some(&renewed_session))
        .unwrap();
    assert!(cancelled.summary.cancel_requested);
    assert_eq!(fixture.store.cancel_investigation(&id).unwrap(), cancelled);
    renewed.revoke();
    denied_session(&mut fixture.store, &id, &renewed_session);
    drop(renewed);
    denied_session(&mut fixture.store, &id, &renewed_session);
    // A denied grant must not begin storage validation or a nested transaction.
    fixture.store.conn.execute_batch("BEGIN").unwrap();
    denied_session(&mut fixture.store, &id, &read_only.session());
    denied_session(&mut fixture.store, &id, &renewed_session);
    fixture.store.conn.execute_batch("ROLLBACK").unwrap();
}
