use super::*;

fn identity(store: &mut JobStore) -> McpRequestIdentity {
    McpRequestIdentity {
        client: store
            .create_mcp_client(std::num::NonZeroUsize::new(4).unwrap())
            .unwrap(),
        normalized_intent_hash: core_prov::content_hash(b"normalized caller intent"),
        maximum_identities: 4,
    }
}

// AC-0322: competing connections serialize lookup and creation in one transaction.
#[test]
fn mcp_nonce_concurrent_retries_create_one_durable_identity() {
    let mut fixture = Fixture::new();
    let identity = identity(&mut fixture.store);
    let path = fixture.directory.path().join("state.db");
    let stores = [
        JobStore::open(&path).unwrap(),
        JobStore::open(&path).unwrap(),
    ];
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = stores
        .into_iter()
        .map(|mut store| {
            let barrier = barrier.clone();
            let client = identity.client.clone();
            let hash = identity.normalized_intent_hash.clone();
            std::thread::spawn(move || {
                let identity = McpRequestIdentity {
                    client,
                    normalized_intent_hash: hash,
                    maximum_identities: 1,
                };
                let request = request("concurrent-mcp", InvestigationProviderMode::Local)
                    .normalized()
                    .unwrap();
                barrier.wait();
                store
                    .start_investigation_internal(
                        &request,
                        Some(&provider(InvestigationProviderMode::Local)),
                        None,
                        false,
                        Some(&identity),
                    )
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.created).count(), 1);
    assert_eq!(results[0].detail, results[1].detail);
    let reopened = JobStore::open(&path).unwrap();
    assert_eq!(
        reopened
            .investigation(&results[0].detail.summary.investigation_id)
            .unwrap(),
        results[0].detail
    );
    let count: i64 = reopened
        .conn
        .query_row("SELECT count(*) FROM investigation_tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

// AC-0324: a new request cannot join foreign or unowned history, even malformed.
#[test]
fn mcp_nonce_admission_rejects_foreign_history_without_writes() {
    let mut fixture = Fixture::new();
    let app = fixture
        .store
        .start_investigation(
            &request("app-history", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    let app_id = app.summary.investigation_id.clone();
    // Queued cancellation settles immediately, preserving capacity for the
    // ownership check instead of failing earlier on a held execution slot.
    fixture.store.cancel_investigation(&app_id).unwrap();
    let identity = identity(&mut fixture.store);
    let owned = fixture
        .store
        .start_investigation_internal(
            &request("owned-history", InvestigationProviderMode::Local)
                .normalized()
                .unwrap(),
            Some(&provider(InvestigationProviderMode::Local)),
            None,
            false,
            Some(&identity),
        )
        .unwrap();
    fixture
        .store
        .conn
        .execute(
            "UPDATE investigation_tasks SET record=X'7b' WHERE id=?1",
            [&app_id],
        )
        .unwrap();
    fixture.store.conn.execute("INSERT INTO investigation_conversations(id,created_at) VALUES ('conversation:empty','2026-10-23T00:00:00Z')", []).unwrap();
    let mut request = request("foreign-followup", InvestigationProviderMode::Local)
        .normalized()
        .unwrap();
    for (conversation, parent) in [
        (Some(app.summary.conversation_id), None),
        (Some(owned.detail.summary.conversation_id), Some(app_id)),
        (Some("conversation:empty".into()), None),
        (Some("conversation:missing".into()), None),
    ] {
        request.conversation_id = conversation;
        request.parent_id = parent;
        let changes = fixture.store.conn.total_changes();
        assert_eq!(
            fixture
                .store
                .start_investigation_internal(
                    &request,
                    Some(&provider(InvestigationProviderMode::Local)),
                    None,
                    false,
                    Some(&identity)
                )
                .unwrap_err(),
            InvestigationStoreError::Missing
        );
        assert_eq!(fixture.store.conn.total_changes(), changes);
    }
}

// AC-0322/0323/0327: storage admission uses host-derived principal identity;
// this fixture does not grant transport authority or invoke any provider.
#[test]
fn mcp_request_namespace_replay_capacity_and_app_collision() {
    let mut fixture = Fixture::new();
    let base = request("same-textual-nonce", InvestigationProviderMode::Local)
        .normalized()
        .unwrap();
    let descriptor = provider(InvestigationProviderMode::Local);
    let app = fixture
        .store
        .start_investigation(&base, Some(&descriptor))
        .unwrap();
    let mut identity = McpRequestIdentity {
        client: fixture
            .store
            .create_mcp_client(std::num::NonZeroUsize::new(2).unwrap())
            .unwrap(),
        normalized_intent_hash: core_prov::content_hash(b"normalized caller intent"),
        maximum_identities: 1,
    };
    let registered = identity.client.clone();
    identity.client = McpClientId::from_host_bytes([0xff; 16]);
    let before = fixture.store.conn.total_changes();
    assert_eq!(
        fixture
            .store
            .start_investigation_internal(&base, Some(&descriptor), None, false, Some(&identity),)
            .unwrap_err(),
        InvestigationStoreError::Missing
    );
    assert_eq!(fixture.store.conn.total_changes(), before);
    identity.client = registered;
    let first = fixture
        .store
        .start_investigation_internal(&base, Some(&descriptor), None, false, Some(&identity))
        .unwrap();
    assert!(first.created);
    assert_ne!(
        first.detail.summary.investigation_id,
        app.detail.summary.investigation_id
    );
    assert_eq!(
        first.detail.origin,
        InvestigationOrigin::Mcp {
            client_id: identity.client.clone()
        }
    );
    // Frozen graph input may change before a retry. Original caller intent wins
    // before unavailable provider and newly reduced principal capacity checks.
    let mut changed_graph = base.clone();
    changed_graph.expected_graph_revision = Some("b".repeat(64));
    identity.maximum_identities = 0;
    let replay = fixture
        .store
        .start_investigation_internal(&changed_graph, None, None, false, Some(&identity))
        .unwrap();
    assert!(!replay.created);
    assert_eq!(replay.detail, first.detail);
    // Leave one global live slot free: the refusal must be the principal
    // capacity, not the independent global concurrency ceiling.
    fixture
        .store
        .cancel_investigation(&app.detail.summary.investigation_id)
        .unwrap();
    let changes = fixture.store.conn.total_changes();
    let mut new_nonce = base.clone();
    new_nonce.request_nonce = "new".into();
    assert_eq!(
        fixture
            .store
            .start_investigation_internal(
                &new_nonce,
                Some(&descriptor),
                None,
                false,
                Some(&identity)
            )
            .unwrap_err(),
        InvestigationStoreError::Capacity
    );
    assert_eq!(fixture.store.conn.total_changes(), changes);
    identity.normalized_intent_hash = core_prov::content_hash(b"different caller intent");
    assert_eq!(
        fixture
            .store
            .start_investigation_internal(&base, None, None, false, Some(&identity))
            .unwrap_err(),
        InvestigationStoreError::Conflict
    );
    fixture
        .store
        .cancel_investigation(&app.detail.summary.investigation_id)
        .unwrap();
    let app_after_cancel = fixture
        .store
        .investigation(&app.detail.summary.investigation_id)
        .unwrap();
    identity.client = fixture
        .store
        .create_mcp_client(std::num::NonZeroUsize::new(2).unwrap())
        .unwrap();
    identity.maximum_identities = 1;
    let other = fixture
        .store
        .start_investigation_internal(&base, Some(&descriptor), None, false, Some(&identity))
        .unwrap();
    assert_ne!(
        other.detail.summary.investigation_id,
        first.detail.summary.investigation_id
    );
    let path = fixture.directory.path().join("state.db");
    fixture.store = JobStore::open(&path).unwrap();
    assert_eq!(
        fixture
            .store
            .investigation(&first.detail.summary.investigation_id)
            .unwrap(),
        first.detail
    );
    assert_eq!(
        fixture
            .store
            .start_investigation(&base, None)
            .unwrap()
            .detail,
        app_after_cancel
    );
}

// AC-0320/0322/0327: live authority, not a caller-provided identity, owns admission.
#[test]
fn mcp_granted_start_derives_identity_and_refuses_revoked_replay() {
    let mut f = Fixture::new();
    let one = std::num::NonZeroUsize::new(1).unwrap();
    let client = f.store.create_mcp_client(one).unwrap();
    let grant = McpTaskActivation::granted(client.clone(), one);
    let session = grant.session();
    let request = request("granted-start", InvestigationProviderMode::Local);
    let provider = provider(InvestigationProviderMode::Local);
    let readonly = McpTaskActivation::default();
    let before = f.store.conn.total_changes();
    assert_eq!(
        f.store
            .start_investigation_access(&request, Some(&provider), Some(&readonly.session()))
            .unwrap_err(),
        InvestigationStoreError::Missing
    );
    assert_eq!(f.store.conn.total_changes(), before);
    let first = f
        .store
        .start_investigation_access(&request, Some(&provider), Some(&session))
        .unwrap();
    assert_eq!(
        first.detail.origin,
        InvestigationOrigin::Mcp { client_id: client }
    );
    let tx = f.store.conn.unchecked_transaction().unwrap();
    let saved = load_task(&tx, &first.detail.summary.investigation_id).unwrap();
    assert_eq!(saved.mcp_intent_hash, Some(request.intent_hash().unwrap()));
    tx.commit().unwrap();
    let replay = f
        .store
        .start_investigation_access(&request, None, Some(&session))
        .unwrap();
    assert!(!replay.created);
    assert_eq!(replay.detail, first.detail);
    let mut changed = request.clone();
    changed.question.push('?');
    assert_eq!(
        f.store
            .start_investigation_access(&changed, None, Some(&session))
            .unwrap_err(),
        InvestigationStoreError::Conflict
    );
    changed = request.clone();
    changed.request_nonce = "second-identity".into();
    assert_eq!(
        f.store
            .start_investigation_access(&changed, Some(&provider), Some(&session))
            .unwrap_err(),
        InvestigationStoreError::Capacity
    );
    let successor = grant.read_only_successor().unwrap();
    let before = f.store.conn.total_changes();
    for denied in [session, successor.session()] {
        assert_eq!(
            f.store
                .start_investigation_access(&request, None, Some(&denied))
                .unwrap_err(),
            InvestigationStoreError::Missing
        );
    }
    assert_eq!(f.store.conn.total_changes(), before);
    let app = f
        .store
        .start_investigation(&request, Some(&provider))
        .unwrap();
    assert_eq!(app.detail.origin, InvestigationOrigin::App);
    assert_ne!(
        app.detail.summary.investigation_id,
        first.detail.summary.investigation_id
    );
}
