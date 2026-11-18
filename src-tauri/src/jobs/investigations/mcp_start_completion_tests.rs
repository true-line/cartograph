use super::*;

// AC-0320/0322/0327: both completion families derive identity under a live grant.
#[test]
fn mcp_granted_completion_start_preserves_families_and_refuses_revocation() {
    // AC-0552: explicit18 retains the same grant, namespace and nonce rules.
    for (next, role) in [
        (false, None),
        (true, Some("domain-analyst@10")),
        (true, Some("domain-analyst@18")),
        (true, Some("domain-analyst@26")),
        (true, Some("domain-analyst@27")),
        (true, Some("domain-analyst@28")),
    ] {
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
        if let Some(role) = role {
            request.schema_version = 7;
            request.specialist_id = role.into();
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
        assert_eq!(first.detail.limits.generation_policy, None);
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
        // AC-0327: releasing routing owners must not skip the full validator.
        tx.execute(
            "UPDATE investigation_tasks SET revision=revision+1 WHERE id=?1",
            [&first.detail.summary.investigation_id],
        )
        .unwrap();
        assert!(matches!(
            load_execution_task(&tx, &first.detail.summary.investigation_id),
            Err(InvestigationStoreError::Invalid)
        ));
        tx.rollback().unwrap();
        let replay = store
            .start_completion_access(&request, None, next, Some(&session))
            .unwrap();
        assert!(!replay.created);
        assert_eq!(replay.detail, first.detail);
        // AC-0322/0324: both families use the same saved-state retry boundary.
        let reopened = JobStore::open(dir.path().join("state.db")).unwrap();
        assert_eq!(
            reopened
                .existing_investigation_completion_access(&request, Some(&session))
                .unwrap(),
            Some(first.detail.clone())
        );
        assert!(
            reopened
                .existing_investigation_completion(&request)
                .unwrap()
                .is_none()
        );
        let foreign = store
            .create_mcp_client(std::num::NonZeroUsize::new(2).unwrap())
            .unwrap();
        let foreign_grant = McpTaskActivation::granted(foreign, one);
        assert!(
            reopened
                .existing_investigation_completion_access(&request, Some(&foreign_grant.session()))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            reopened
                .existing_investigation_completion_access(&request, Some(&readonly.session()))
                .unwrap_err(),
            InvestigationStoreError::Missing
        );
        let mut changed = request.clone();
        changed.question.push('?');
        assert_eq!(
            reopened
                .existing_investigation_completion_access(&changed, Some(&session))
                .unwrap_err(),
            InvestigationStoreError::Conflict
        );
        assert_eq!(
            store
                .start_completion_access(&changed, None, next, Some(&session))
                .unwrap_err(),
            InvestigationStoreError::Conflict
        );
        grant.revoke();
        assert_eq!(
            reopened
                .existing_investigation_completion_access(&request, Some(&session))
                .unwrap_err(),
            InvestigationStoreError::Missing
        );
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
