use super::*;

// AC-0320/0322/0327: both completion families derive identity under a live grant.
#[test]
fn mcp_granted_completion_start_preserves_families_and_refuses_revocation() {
    for next in [false, true] {
        let source = if next {
            Fixture::with_family_preparation(
                InvestigationProviderMode::Local,
                &INVESTIGATION_V2_POLICY,
                true,
                false,
            )
        } else {
            Fixture::new()
        };
        let provider = source.store.investigation(&source.id).unwrap().provider;
        let mut request = source.request.clone();
        if next {
            request.schema_version = 7;
            request.specialist_id = "domain-analyst@10".into();
        }
        let dir = tempfile::tempdir().unwrap();
        let mut store = JobStore::open(dir.path().join("state.db")).unwrap();
        let one = std::num::NonZeroUsize::new(1).unwrap();
        let client = store.create_mcp_client(one).unwrap();
        let grant = McpTaskActivation::granted(client.clone(), one);
        let session = grant.session();
        let readonly = McpTaskActivation::default();
        let before = store.conn.total_changes();
        assert_eq!(
            store
                .start_completion_access(&request, Some(&provider), next, Some(&readonly.session()))
                .unwrap_err(),
            InvestigationStoreError::Missing
        );
        assert_eq!(store.conn.total_changes(), before);
        let first = store
            .start_completion_access(&request, Some(&provider), next, Some(&session))
            .unwrap();
        assert_eq!(
            first.detail.origin,
            InvestigationOrigin::Mcp { client_id: client }
        );
        let policy = InvestigationPolicy::for_profile(&request.limit_profile).unwrap();
        let tx = store.conn.unchecked_transaction().unwrap();
        let saved = load_execution_task(&tx, &first.detail.summary.investigation_id).unwrap();
        assert_eq!(
            saved.mcp_intent_hash,
            Some(request.intent_hash_with_policy(policy).unwrap())
        );
        tx.commit().unwrap();
        let replay = store
            .start_completion_access(&request, None, next, Some(&session))
            .unwrap();
        assert!(!replay.created);
        assert_eq!(replay.detail, first.detail);
        let mut changed = request.clone();
        changed.question.push('?');
        assert_eq!(
            store
                .start_completion_access(&changed, None, next, Some(&session))
                .unwrap_err(),
            InvestigationStoreError::Conflict
        );
        grant.revoke();
        let before = store.conn.total_changes();
        assert_eq!(
            store
                .start_completion_access(&request, None, next, Some(&session))
                .unwrap_err(),
            InvestigationStoreError::Missing
        );
        assert_eq!(store.conn.total_changes(), before);
    }
}
