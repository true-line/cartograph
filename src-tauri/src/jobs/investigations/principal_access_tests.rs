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
    let changes = store.conn.total_changes();
    assert_eq!(
        store.investigation_access(id, Some(client)).unwrap_err(),
        InvestigationStoreError::Missing
    );
    assert_eq!(
        store
            .investigation_result_access(id, Some(client))
            .unwrap_err(),
        InvestigationStoreError::Missing
    );
    // Ownership also precedes validation of the event cursor and family routing.
    assert_eq!(
        store
            .investigation_events_access(id, u64::MAX, Some(client))
            .unwrap_err(),
        InvestigationStoreError::Missing
    );
    for next in [false, true] {
        assert_eq!(
            store
                .cancel_investigation_access(id, next, Some(client))
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
            .investigation_access(&id, Some(&client))
            .unwrap(),
        fixture.store.investigation(&id).unwrap()
    );
    assert_eq!(
        fixture
            .store
            .investigation_result_access(&id, Some(&client))
            .unwrap(),
        None
    );
    assert_eq!(
        fixture
            .store
            .investigation_events_access(&id, 0, Some(&client))
            .unwrap(),
        fixture.store.investigation_events(&id, 0).unwrap()
    );
    let cancelled = fixture
        .store
        .cancel_investigation_access(&id, false, Some(&client))
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
            .investigation_result_access(&id, Some(&client))
            .unwrap(),
        Some(result)
    );
    assert_eq!(
        fixture
            .store
            .investigation_result_access(&id, Some(&client))
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
