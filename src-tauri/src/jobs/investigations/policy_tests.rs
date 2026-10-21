use super::*;

#[test]
fn investigation_saved_policy_rejects_rehashed_tampering_after_reopen() {
    // AC-0188: a valid record hash does not authorize caller-selected limits.
    let mut fixture = Fixture::new();
    let detail = fixture
        .store
        .start_investigation(
            &request("saved-policy", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    let id = &detail.summary.investigation_id;
    let original: Vec<u8> = fixture
        .store
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    for mutation in ["unknown_profile", "more_calls", "less_tokens"] {
        let mut task: StoredTask = serde_json::from_slice(&original).unwrap();
        match mutation {
            "unknown_profile" => task.detail.limits.profile = "investigation-v2".into(),
            "more_calls" => task.detail.limits.model_invocations += 1,
            "less_tokens" => task.detail.limits.generated_token_reservations -= 1,
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&task).unwrap();
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
                params![id, bytes, core_prov::content_hash(&bytes)],
            )
            .unwrap();
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(
            reopened.investigation(id).unwrap_err(),
            InvestigationStoreError::Invalid,
            "{mutation}"
        );
    }
    fixture
        .store
        .conn
        .execute(
            "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
            params![id, original, core_prov::content_hash(&original)],
        )
        .unwrap();
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(id).unwrap().limits, detail.limits);
}
