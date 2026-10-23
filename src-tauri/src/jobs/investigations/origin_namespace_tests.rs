use super::*;

fn body(store: &JobStore, id: &str) -> Vec<u8> {
    store
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}
fn replace(store: &JobStore, task: &StoredTask, namespace: &str) {
    let bytes = storage::encode(task, MAX_RECORD_BYTES).unwrap();
    store.conn.execute("UPDATE investigation_tasks SET record=?2,record_hash=?3,request_namespace=?4 WHERE id=?1",
        params![task.detail.summary.investigation_id, bytes, core_prov::content_hash(&bytes), namespace]).unwrap();
}

// AC-0322/0323: attribution is durable metadata, not authority to execute.
#[test]
fn task_origin_namespace_preserves_app_bytes_and_rejects_mismatches() {
    let mut fixture = Fixture::new();
    let (id, _) = fixture.start("origin-namespace", InvestigationProviderMode::Local);
    let original = body(&fixture.store, &id);
    let mut task: StoredTask = serde_json::from_slice(&original).unwrap();
    assert!(task.mcp_intent_hash.is_none());
    assert_eq!(storage::encode(&task, MAX_RECORD_BYTES).unwrap(), original);
    assert!(
        !serde_json::from_slice::<serde_json::Value>(&original)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("mcp_intent_hash")
    );
    let before = storage::actual_stored_bytes(&fixture.store.conn, Some(&id)).unwrap();
    task.detail.origin = InvestigationOrigin::Mcp {
        client_id: McpClientId::from_host_bytes([0xab; 16]),
    };
    task.mcp_intent_hash = Some("a".repeat(64));
    let namespace = task.detail.origin.request_namespace();
    replace(&fixture.store, &task, &namespace);
    assert_eq!(
        fixture.store.investigation(&id).unwrap().origin,
        task.detail.origin
    );
    let after = storage::actual_stored_bytes(&fixture.store.conn, Some(&id)).unwrap();
    assert_eq!(
        after - before,
        body(&fixture.store, &id).len() - original.len() + 33
    );
    for invalid in [
        "app".into(),
        format!("mcp:{}", "b".repeat(32)),
        "x".repeat(37),
        "é".repeat(32),
    ] {
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_tasks SET request_namespace=?2 WHERE id=?1",
                params![id, invalid],
            )
            .unwrap();
        assert_eq!(
            fixture.store.investigation(&id).unwrap_err(),
            InvestigationStoreError::Invalid
        );
    }
    replace(&fixture.store, &task, &namespace);
    let mut changed = task.clone();
    changed.detail.origin = InvestigationOrigin::App;
    changed.mcp_intent_hash = None;
    assert!(
        storage::save_task(
            &fixture.store.conn,
            &changed,
            changed.detail.summary.revision,
            false
        )
        .is_err()
    );
    assert_eq!(
        fixture.store.investigation(&id).unwrap().origin,
        task.detail.origin
    );
}

#[test]
fn task_origin_namespace_rejects_invalid_intent_pairings() {
    let mut fixture = Fixture::new();
    let (id, _) = fixture.start("origin-intent", InvestigationProviderMode::Local);
    let mut task: StoredTask = serde_json::from_slice(&body(&fixture.store, &id)).unwrap();
    task.mcp_intent_hash = Some("a".repeat(64));
    replace(&fixture.store, &task, "app");
    assert_eq!(
        fixture.store.investigation(&id).unwrap_err(),
        InvestigationStoreError::Invalid
    );
    task.detail.origin = InvestigationOrigin::Mcp {
        client_id: McpClientId::from_host_bytes([1; 16]),
    };
    let namespace = task.detail.origin.request_namespace();
    for invalid in [None, Some("short".into()), Some("Z".repeat(64))] {
        task.mcp_intent_hash = invalid;
        replace(&fixture.store, &task, &namespace);
        assert_eq!(
            fixture.store.investigation(&id).unwrap_err(),
            InvestigationStoreError::Invalid
        );
    }
}
